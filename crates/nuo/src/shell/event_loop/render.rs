//! The per-frame draw routine for the TUI event loop, extracted from
//! `run_app_loop`'s `if needs_draw` stage (it was a ~1000-line closure).
use crate::shell::completion::{CompletionKind, completion_anchor, resolved_slash_command_len};
use crate::shell::composer::{ComposerProps, ComposerText};
use crate::shell::overlays::provider_delete_confirm::ProviderDeleteChoice as ConfirmChoice;
use crate::shell::render;
use crate::shell::surfaces::{DialogKind, OverlaySurface, SceneKind, SheetKind};
use crate::shell::ui::UiKey;
use crate::shell::{App, ProviderDeleteChoice};
use nuo_ui_primitives::primitives::Recess;
use nuo_view::model::document::TranscriptMessage;
use nuo_view::model::layout::LayoutMap;

use super::actions::effective_reasoning_effort;
use super::transcript::display_status;

/// Loop stage: the per-frame draw. Paints the chrome (startup picker,
/// transcript, hint bar, composer, completion popup), recesses the live
/// surface for the open modal, then draws the
/// active modal panel, persisting per-frame layout state back onto `app`.
/// Invoked through `Terminal::stage` (bottom-follow measurement pass) or
/// `Terminal::draw`; extracted verbatim from the `render_frame` closure.
pub(crate) fn render_frame(app: &mut App, f: &mut nuotc::Frame<'_>, viewed_session_id: &str) {
    let mut ui = std::mem::take(&mut app.transcript.ui);
    compose_frame(app, f, viewed_session_id, &mut ui);
    app.transcript.ui = ui;
}

/// Compose one frame. The orchestrator owns only modality pre-registration
/// and the ordering of the surface families; each family below is a verbatim
/// slice of the historical single-function body, so draw order, hit-map
/// registration, and styling are unchanged.
fn compose_frame(
    app: &mut App,
    f: &mut nuotc::Frame<'_>,
    viewed_session_id: &str,
    ui: &mut crate::shell::ui::ComponentTree,
) {
    ui.begin(f.area());
    // Modality is scene-owned: a modal owns keys because the
    // mounted component declares `Modal` policy, not because an app flag says
    // so. The mount here pre-registers the modal at viewport size; the modal
    // renderer places it at its drawn rect further down.
    if let Some(overlay) = app.surfaces.surfaces.active_overlay() {
        ui.mount(UiKey::Overlay(overlay), f.area());
    }
    if app.surfaces.config_dropdown.is_some() {
        ui.mount(UiKey::ConfigDropdown, f.area());
    }
    let mut layout_map = LayoutMap::new();

    if draw_startup_surface(app, f, ui, &mut layout_map) {
        return;
    }

    let transcript = draw_transcript_surface(app, f, viewed_session_id, ui, &mut layout_map);
    let slot = ComposerSlot {
        input_rect: transcript.input_rect,
        hint_rect: transcript.hint_rect,
    };

    draw_model_bar_surface(app, f, viewed_session_id, ui, slot);
    draw_composer_surface(app, f, viewed_session_id, ui, &mut layout_map, slot);
    persist_transcript_layout(app, ui, &transcript);
    draw_interaction_sheets(app, f, ui, slot);
    draw_completion_popup(app, f, ui, &mut layout_map, slot);

    let surface =
        draw_recessed_surface(app, f, viewed_session_id, ui, &mut layout_map, &transcript);

    draw_provider_delete_confirm(app, f, ui);
    draw_toasts(app, f);
    finish_frame(app, ui, &mut layout_map, surface);
}

/// The composer/hint slot resolved by the transcript pass and shared by every
/// bottom-anchored surface drawn after it.
#[derive(Clone, Copy)]
struct ComposerSlot {
    input_rect: nuotc::Rect,
    hint_rect: nuotc::Rect,
}

/// Measurements a scene/overlay pass reports to the frame tail.
struct SurfaceDraw {
    modal_rect: Option<nuotc::Rect>,
    dashboard_list_body_height: Option<u16>,
}

/// Startup guards. The PreAttach interstitial and the isolated `nuo attach`
/// sessions picker both own the whole frame and suppress chat chrome.
/// Returns `true` when one of them painted, in which case the frame is done.
fn draw_startup_surface(
    app: &mut App,
    f: &mut nuotc::Frame<'_>,
    ui: &mut crate::shell::ui::ComponentTree,
    layout_map: &mut LayoutMap,
) -> bool {
    // PreAttach interstitial takes over the terminal before
    // any chat/composer/transcript chrome is painted. The render
    // early-returns, exactly like the SessionsPicker guard below, so
    // nothing downstream (chrome, composer, panels) runs while the
    // workspace trust decision is still pending. The only interactive
    // surface on this frame is the trust prompt rendered by
    // `draw_pre_attach`.
    if let Some(pre_attach_state) = app.surfaces.pre_attach.as_ref() {
        crate::shell::pre_attach::draw_pre_attach(f, pre_attach_state, &app.surfaces.theme);
        ui.mount(crate::shell::ui::UiKey::PreAttach, f.area());
        // Layout map / modal rect / modal hit map stay empty — there
        // is no chrome to interact with, and the PreAttach surface
        // owns its own hit-testing (it does not consume LayoutMap).
        ui.stage_document(std::mem::take(layout_map));
        return true;
    }

    if app.surfaces.startup_overlay == crate::shell::StartupOverlay::SessionsPicker
        && app.active_dialog() == Some(DialogKind::Threads)
    {
        // `nuo attach` (no id): initial launch opens ONLY the sessions picker
        // on a clean background. Do not open/render the chat interface, empty state,
        // composer input box, status bar, or header components until a session is selected.
        f.render_widget(
            &mut nuotc::widgets::Block::default()
                .style(nuotc::Style::default().bg(app.surfaces.theme.app_bg)),
            f.area(),
        );

        let spinner_phase = (app.transcript.spinner_epoch.elapsed().as_millis() / 100) as usize;
        let projected_count = crate::shell::overlays::session::project_session_rows(
            &app.session.sessions_overview,
            Some(
                &app.surfaces
                    .surfaces
                    .dlg::<crate::shell::surfaces::SessionsDialog>()
                    .expanded,
            ),
        )
        .len();
        let startup_picker =
            app.surfaces.startup_overlay == crate::shell::StartupOverlay::SessionsPicker;
        let sessions = app
            .surfaces
            .surfaces
            .dlg_mut::<crate::shell::surfaces::SessionsDialog>();
        let drawn_modal_rect = render::draw_sessions_modal(
            f,
            crate::shell::overlays::session::SessionsModalProps {
                sessions: &app.session.sessions_overview,
                expanded_sessions: Some(&sessions.expanded),
                selected: sessions.index.min(projected_count.saturating_sub(1)),
                scroll: &mut sessions.scroll,
                follow: sessions.follow,
                startup_picker,
                spinner_phase,
                session_info_detail: sessions.info_detail,
                session_detail: app.session.session_detail.as_ref(),
                session_info_scroll: &mut sessions.info_scroll,
                sessions_loading: sessions.loading,
                running_sessions: Some(&app.session.running_sessions),
            },
            &app.surfaces.theme,
            &app.transcript.selection,
            layout_map,
        );

        ui.stage_document(std::mem::take(layout_map));
        app.transcript.modal_body_height = drawn_modal_rect.height.saturating_sub(
            nuo_ui_primitives::primitives::modal_chrome_rows(
                nuo_ui_primitives::primitives::ModalSpec {
                    width_percent: 0,
                    header: true,
                    footer: true,
                },
            ),
        );
        if let Some(overlay) = app.surfaces.surfaces.active_overlay() {
            ui.mount(UiKey::Overlay(overlay), drawn_modal_rect);
        }
        return true;
    }

    false
}

/// Transcript family: head band, message stream, footer placement, and every
/// hit-test rect the transcript pass registered. Also derives the per-view
/// chrome state (status, transport setback, queue preview, empty-state
/// guidance) that only this surface consumes.
fn draw_transcript_surface(
    app: &mut App,
    f: &mut nuotc::Frame<'_>,
    viewed_session_id: &str,
    ui: &mut crate::shell::ui::ComponentTree,
    layout_map: &mut LayoutMap,
) -> render::TranscriptRender {
    // Borrow the height cache out of `app` for the duration of the draw:
    // `view_messages` borrows `app` immutably below, so the cache cannot
    // also be reached through `app` at the same time. It is restored once
    // `view_messages` is no longer borrowed (see below).
    let mut height_cache = std::mem::take(&mut app.transcript.layout_height_cache);
    // View-scoped chrome: render the phase of whichever session the user is
    // viewing, from that session's own chrome entry. This is the per-view
    // activity-bar fix: the displayed bar tracks the *viewed* session, never a
    // global blend. A pending permission
    // sheet overrides the phase with its gate label (warning hue downstream).
    let viewed_chrome = app.viewed_chrome();
    let gate_phase = app
        .human
        .pending_permission
        .is_some()
        .then_some(nuo_view::phase::Phase::AwaitingUser);
    let status = if let Some(ref target) = app.session.switching_session {
        format!("loading session {target}…")
    } else if app.link.link_down {
        // Dead-link chrome state (D6): the server link is gone, so
        // user intents cannot be delivered. The live-status bar is the
        // visible anchor; it stays until the process exits, because nothing
        // can acknowledge recovery.
        "server link lost".to_string()
    } else {
        let base_status = display_status(
            app.session.loop_status,
            gate_phase.as_ref().or(viewed_chrome.phase.as_ref()),
        );
        // Prioritize system lifecycle (e.g. MCP connecting progress) when round is idle.
        if (base_status.is_empty() || base_status == "idle") && gate_phase.is_none() {
            if let Some(mcp_progress) = mcp_connecting_status(app) {
                mcp_progress
            } else {
                base_status
            }
        } else {
            base_status
        }
    };
    // Transport-setback clause: rides beside the status label (never in its
    // slot), counting down while a provider retry backs off.
    //
    // Read from the *viewed* session's chrome: the clause annotates
    // that session's phase, so a background session backing off against a
    // rate-limited upstream cannot paint a countdown onto the primary's bar.
    // Its lifetime is already over if the phase moved on — `set_phase` retires
    // it — so this needs no staleness check of its own.
    // Passed as the typed state: the renderer owns the choice between the full
    // and compact forms, and both come from this value.
    let backoff = viewed_chrome.transport_setback.as_ref();
    // Compute the displayed input text first so the transcript layout can
    // reserve the right height for a wrapping, growing input box. Only the text
    // is masked: the box's height is a function of the wrapped row count, so no
    // caret offset has to be mapped through the mask any more — the composer
    // itself draws its caret from the real buffer.
    let masked_input = if app.surfaces.surfaces.contains_sheet(SheetKind::ModelEditor)
        && app.provider_editor.editor_field == 0
    {
        // Mask the API key everywhere it could be rendered (the editor
        // field itself, and any layout pass that inspects the input).
        "•".repeat(app.composer.input.chars().count())
    } else {
        app.composer.input.clone()
    };
    let is_fullscreen_scene = matches!(
        app.current_scene(),
        SceneKind::Dashboard | SceneKind::Settings | SceneKind::CommandLog
    );
    let chrome_hidden = is_fullscreen_scene;
    // When zoomed into a Subagent, render its child messages and
    // show a contextual first-row header; otherwise render the
    // root thread.
    let view_messages = app.focused_messages();
    let subagent_bar = app.transcript.focus_stack.last().and_then(|current| {
        let tasks: Vec<&TranscriptMessage> = app
            .transcript
            .messages
            .iter()
            .filter(|message| message.is_subagent_task())
            .collect();
        let idx = tasks
            .iter()
            .position(|message| message.tool_step_call_id() == Some(current.call_id.as_str()))?;
        Some(render::SubagentBarInfo {
            role: tasks.get(idx)?.subagent_role(),
            label: tasks.get(idx)?.subagent_description(),
            index: idx + 1,
            total: tasks.len(),
        })
    });
    let (scene_kind, scene_context, scene_context_warn): (
        crate::shell::surfaces::SceneKind,
        Option<String>,
        bool,
    ) = if let Some(bar) = subagent_bar.as_ref() {
        let role = bar
            .role
            .as_deref()
            .map(|role| format!("[{}]", role.to_uppercase()))
            .unwrap_or_default();
        let count = if bar.total > 1 {
            format!(" ({}/{})", bar.index, bar.total)
        } else {
            String::new()
        };
        let context = if role.is_empty() {
            format!("{}{count}", bar.label)
        } else {
            format!("{role} {}{count}", bar.label)
        };
        (
            crate::shell::surfaces::SceneKind::Subagent,
            Some(context),
            false,
        )
    } else {
        (
            crate::shell::surfaces::SceneKind::Thread,
            app.viewed_thread_title(),
            false,
        )
    };
    let (breadcrumbs, can_back, can_forward) = if let Some(tab) = app.surfaces.surfaces.active_tab()
    {
        (Some(tab.breadcrumbs()), tab.can_back(), tab.can_forward())
    } else {
        (None, false, false)
    };
    let page_hints = render::ViewHints {
        kind: scene_kind,
        context: scene_context.as_deref(),
        context_warn: scene_context_warn,
        breadcrumbs,
        can_back,
        can_forward,
    };
    // Empty-state guidance policy (ADR-0057/0104): the app shell picks the
    // variant, the view paints it. A setup blocker beats the tour — nothing
    // rotates until a keyed provider exists. The blocker reads from
    // `provider_picker` rows (a row exists ⇒ the provider is configured;
    // `key_status` refines key readiness), mirroring what `/connections`
    // manages, so the nudge clears the moment the user fixes the real thing.
    // An empty snapshot means "not synced yet" — the server's startup
    // snapshot arrives within the first loop iterations — so the tour
    // renders in that window rather than flashing a false no-provider
    // warning at an already-configured user. A genuinely provider-less
    // install is indistinguishable until its snapshot lands; the cost is a
    // few tour frames before the blocker appears, never a false warning.
    let has_keyed_provider = app.surfaces.provider_picker.rows.iter().any(|row| {
        row.key_ready
            || app
                .surfaces
                .key_status
                .get(&row.id)
                .copied()
                .unwrap_or(true)
    }) || app.surfaces.provider_picker.rows.is_empty();
    let guidance = if let Some(ref target) = app.session.switching_session {
        render::EmptyStateGuidance::LoadingSession(target.clone())
    } else if has_keyed_provider {
        render::EmptyStateGuidance::Tour
    } else {
        render::EmptyStateGuidance::NeedsProvider
    };
    // Suppress the hover affordance whenever a full-overlay modal is
    // open so no stale highlight bleeds through. The foreground
    // permission sheet keeps the transcript interactive, so it is
    // exempted; a coexisting modal covers it and restores the suppression.
    let chrome_interactive = app.surfaces.surfaces.active_overlay().is_none()
        && app
            .active_sheet()
            .is_none_or(|kind| kind == crate::shell::sheet::SheetKind::Permission);
    // Project the viewed session's outbox into the small view the activity
    // strip renders. Dispatch order (front pops first) is preserved, so the
    // strip previews the genuine next item to ship. The items are owned
    // snapshots so the strip/modal do not borrow `app` (which is mutated again
    // right after the draw closure).
    //
    // One outbox definition feeds the strip and the modal (see
    // `App::outbox_projection`): the preview cannot name an item the modal
    // does not list, and a pending steer (a `Steering` row) is an outbox item
    // like any other — it is simply never dispatched as a fresh round
    // (ADR-0137).
    let queue_items: Vec<render::QueueItemProps> = app
        .outbox_projection(viewed_session_id)
        .into_iter()
        .map(|item| render::QueueItemProps {
            queued_at_ms: item.queued_at_ms,
            text: item.text.clone(),
            is_steer: item.state == crate::shell::app::QueuedDispatchState::Steering,
        })
        .collect();
    let transcript_render = ui.paint(f, UiKey::Root, |f| {
        render::draw_transcript(
            f,
            layout_map,
            render::TranscriptProps {
                messages: view_messages,
                scroll: app.transcript.scroll,
                selection: &app.transcript.selection,
                cell_selection: app.transcript.drag.cell_info.as_ref(),
                activity: &status,
                backoff,
                // A pending permission request forces the activity bar on (and
                // tints it warning) so it stays the visible anchor above the
                // permission sheet even if the loop has gone idle.
                awaiting_permission: app.human.pending_permission.is_some(),
                // ~100ms per phase keeps one breathing cycle near 1.2s
                // (SPINNER_PHASES steps); `breathing_color` wraps modulo.
                spinner_phase: (app.transcript.spinner_epoch.elapsed().as_millis() / 100) as usize,
                input: &masked_input,
                chrome_hidden,
                queue_bar: render::QueueBarProps {
                    items: &queue_items,
                    // A real condition, not a derived constant: the driver is
                    // holding this session's queue.
                    blocked: app.pending_count(viewed_session_id) > 0
                        && app.is_queue_blocked(viewed_session_id),
                    // The legend's keycap comes from the registry, never a
                    // literal: the bar can only advertise a chord that fires
                    expand_key: app
                        .surfaces
                        .key_overrides
                        .effective_binding(crate::shell::keymap::CommandId::OpenQueue),
                },
                tasks_bar: render::TasksBarProps {
                    tasks: &app.link.background_tasks,
                },
                // The armed interrupt repeats the *resolved* chord, so remapping
                // `CancelOrBack` moves the keycap rather than leaving a stale one.
                interrupt_key: app
                    .surfaces
                    .key_overrides
                    .effective_binding(crate::shell::keymap::CommandId::CancelOrBack),
                subagent_bar,
                page_hints: Some(page_hints),
                tab_bar: Some(render::TabBarProps {
                    tabs: Some(app.surfaces.surfaces.tabs()),
                    active_tab: app.surfaces.surfaces.active_tab_index(),
                    unread_faults: if app.has_unread_notice_fault() {
                        app.unread_notices()
                    } else {
                        0
                    },
                }),
                thread_id: Some(viewed_session_id),
                thread_context: Some(render::ThreadContext {
                    role: app.session.current_role.as_deref(),
                    workspace: &app.session.current_workspace,
                    unattended: app.session.unattended,
                    confined: app.session.confined,
                }),
                // View-scoped: the elapsed-timer origin belongs to the viewed
                // session's round (a background view times its own round, not
                // the primary's).
                round_started_at: viewed_chrome.round_started_at,
                hovered_step: chrome_interactive
                    .then_some(app.transcript.hovered_step)
                    .flatten(),
                focused_entry: chrome_interactive
                    .then_some(app.transcript.focus.entry())
                    .flatten(),
                focus_plane: if app.transcript.focus.is_transcript() {
                    render::FocusPlane::Transcript
                } else {
                    render::FocusPlane::Composer
                },
                logo: app.surfaces.logo.as_deref(),
                guidance,
                carousel_index: nuo_view::empty_state::carousel_page_for(
                    app.transcript.carousel_epoch.elapsed().as_millis(),
                ),
                theme: &app.surfaces.theme,
                layout: app.transcript.transcript_layout,
                height_cache: Some(&mut height_cache),
            },
        )
    });
    // The transcript draw is the only consumer of the height cache; restore
    // it immediately so the rest of the frame sees the refreshed entries.
    app.transcript.layout_height_cache = height_cache;
    for (key, rect) in &transcript_render.footer.rows {
        let key = match key {
            render::FooterRowId::TopGap => continue,
            render::FooterRowId::ActivityStrip => UiKey::Activity,
            render::FooterRowId::Composer => UiKey::Composer,
            render::FooterRowId::ModelBar => UiKey::ModelBar,
        };
        ui.mount(key, *rect);
    }
    if let Some(rect) = transcript_render.tab_bar_rect {
        ui.mount(UiKey::TabBar, rect);
    }
    for (idx, tab_rect) in &transcript_render.tab_rects {
        ui.mount(UiKey::TabBarItem(*idx), *tab_rect);
    }
    if let Some(rect) = transcript_render.tab_prev_rect {
        ui.mount(UiKey::TabBarPrev, rect);
    }
    if let Some(rect) = transcript_render.tab_next_rect {
        ui.mount(UiKey::TabBarNext, rect);
    }
    if let Some(rect) = transcript_render.tab_overflow_rect {
        ui.mount(UiKey::TabBarOverview, rect);
    }
    if let Some(rect) = transcript_render.tab_notices_rect {
        ui.mount(UiKey::NoticesBadge, rect);
    }
    if let Some(rect) = transcript_render.tab_help_rect {
        ui.mount(UiKey::HelpBadge, rect);
    }
    if let Some(rect) = transcript_render.scene_bar_rect {
        ui.mount(UiKey::SceneBar, rect);
    }
    if let Some(rect) = transcript_render.scene_nav_rects.back {
        ui.mount(UiKey::SceneHistoryBack, rect);
    }
    if let Some(rect) = transcript_render.scene_nav_rects.forward {
        ui.mount(UiKey::SceneHistoryForward, rect);
    }
    for (idx, crumb_rect) in &transcript_render.scene_nav_rects.crumbs {
        ui.mount(UiKey::SceneBreadcrumb(*idx), *crumb_rect);
    }
    if let Some(rect) = layout_map.transcript_content_rect() {
        ui.mount(UiKey::Transcript, rect);
    }

    transcript_render
}

/// Footer hint family: the input-action hint line, with model, provider,
/// reasoning-effort, and context-window metadata on the right.
fn draw_model_bar_surface(
    app: &App,
    f: &mut nuotc::Frame<'_>,
    viewed_session_id: &str,
    ui: &mut crate::shell::ui::ComponentTree,
    slot: ComposerSlot,
) {
    let hint_rect = slot.hint_rect;
    let viewed_chrome = app.viewed_chrome();
    let is_fullscreen_scene = matches!(
        app.current_scene(),
        SceneKind::Dashboard | SceneKind::Settings | SceneKind::CommandLog
    );
    let chrome_hidden = is_fullscreen_scene;
    // The input-action hint bar (with model/context metadata on
    // the right) lives directly below the input box. It is drawn
    // before the composer because it borrows `view_messages` (an
    // immutable borrow of `app`) while `draw_composer` needs a
    // mutable borrow of `app.composer.input_scroll`.
    // The permission sheet takes over the hint line as well as the
    // input box, so suppress the hint bar while it is open.
    if !chrome_hidden
        && hint_rect.height > 0
        && app.active_sheet() != Some(crate::shell::sheet::SheetKind::Permission)
    {
        // Resolve the active model's effective reasoning effort for
        // the hint bar's `◆ {effort}` tag. Reads the same per-model
        // channel info the `/models` picker uses
        // (`ProviderModelInfo { effort, thinking }`), then applies
        // the ADR-0046 per-protocol gating: Anthropic effort shows
        // only while thinking is opted in; OpenAI effort (a
        // standalone knob with no separate thinking field) shows
        // whenever the model exposes one; Google never. `None`
        // otherwise — non-reasoning models keep the bar quiet.
        let active_provider_row = app
            .surfaces
            .provider_picker
            .rows
            .iter()
            .find(|row| row.id == app.session.current_provider);
        // The `@<instance>` suffix after the model name — the
        // instance's display name, so identical models served by
        // different instances stay attributable.
        let hint_instance = active_provider_row.map(|row| row.name.as_str());
        // The catalog resolved this route's label once (ADR-0081); the bar
        // draws it name-first and falls back to the wire id. Projections never
        // re-derive typography — a namespaced relay id like
        // `deepseek/deepseek-v4.1-flash` must read `DeepSeek V4.1 Flash` here
        // exactly as it does in the picker.
        let hint_label = app
            .active_model_info()
            .and_then(|info| info.name.as_deref())
            .filter(|label| !label.is_empty());
        let hint_reasoning = effective_reasoning_effort(app);
        let active_info = app.active_model_info();
        let in_models = active_provider_row
            .is_none_or(|row| row.models.iter().any(|m| m == &app.session.current_model));
        let model_usable = active_info
            .and_then(|info| info.availability.as_ref())
            .is_none_or(|avail| avail.usable);
        let model_available = in_models && model_usable;
        let busy = app.session.running_sessions.contains(viewed_session_id);
        let _ = busy; // keys-row input; consumed by the composer draw below
        // `/retry` affordance: offered exactly while the viewed
        // session has a stopped round parked for retry — mirrored from the
        // session-scoped harness snapshot into `SessionChrome`, never
        // re-derived by scanning the transcript (an error notice can follow
        // a completed round, and a compaction can drop the notice entirely).
        let can_retry = !busy && viewed_chrome.can_retry;
        let _ = can_retry; // retry affordance now renders on the composer keys row
        let model_bar_rects = ui.paint(f, UiKey::ModelBar, |f| {
            render::draw_model_bar(
                f,
                hint_rect,
                render::ModelBarProps {
                    current_model: &app.session.current_model,
                    model_label: hint_label,
                    model_available,
                    provider_name: hint_instance,
                    reasoning_effort: hint_reasoning,
                    context_tokens: app.telemetry.context_tokens.map(|snapshot| snapshot.tokens),
                    context_window: app.active_model_context_window(),
                },
                &app.surfaces.theme,
                &app.surfaces.key_overrides,
            )
        });
        for (key, rect) in [
            (UiKey::Context, model_bar_rects.context),
            (UiKey::Connection, model_bar_rects.connection),
        ] {
            if let Some(rect) = rect {
                ui.mount(key, rect);
            }
        }
    }
}

/// Composer family: the inline permission sheet that replaces the composer
/// slot, or the live composer (resolved-command highlighting, ghost text,
/// hints, images, and pastes).
fn draw_composer_surface(
    app: &mut App,
    f: &mut nuotc::Frame<'_>,
    viewed_session_id: &str,
    ui: &mut crate::shell::ui::ComponentTree,
    layout_map: &mut LayoutMap,
    slot: ComposerSlot,
) {
    let input_rect = slot.input_rect;
    let hint_rect = slot.hint_rect;
    let viewed_chrome = app.viewed_chrome();
    let is_fullscreen_scene = matches!(
        app.current_scene(),
        SceneKind::Dashboard | SceneKind::Settings | SceneKind::CommandLog
    );
    let has_overlay = app.surfaces.surfaces.active_overlay().is_some();
    let chrome_hidden = is_fullscreen_scene;
    let scene_owns_caret =
        app.caret_visible() && app.caret_owner() == crate::shell::CaretOwner::Composer;
    // The input box is only shown when no overlay modal is open. The
    // `focused` flag drops the panel to its dim "blurred" palette and
    // hides the caret whenever keyboard focus is on the thread
    // stream (Browse zone), so the user can see at a glance which
    // surface the next keypress will land on. A pending permission
    // request replaces the composer with the inline permission sheet.
    if !chrome_hidden {
        if app.active_sheet() == Some(crate::shell::sheet::SheetKind::Permission) {
            if let Some(request) = app.human.pending_permission.as_ref() {
                // Extend the slot down by the composer/hint gap plus
                // the hint-line height so the sheet also covers
                // (replaces) the bar below the input.
                let permission_rect = nuotc::Rect::new(
                    input_rect.x,
                    input_rect.y,
                    input_rect.width,
                    input_rect.height
                        + nuo_ui_primitives::design::COMPOSER_HINT_GAP_ROWS
                        + hint_rect.height,
                );
                let max_scroll = render::draw_permission_sheet(
                    f,
                    ui,
                    request,
                    app.surfaces.modal_index,
                    app.human.permission_confirm_always,
                    app.human.permission_show_details,
                    app.human.permission_scroll,
                    app.human.pending_permission_depth,
                    permission_rect,
                    &app.surfaces.theme,
                    &app.transcript.selection,
                    layout_map,
                );
                app.human.permission_max_scroll = max_scroll;
                app.human.permission_scroll = app
                    .human
                    .permission_scroll
                    .min(app.human.permission_max_scroll);
            }
        } else if matches!(
            app.active_dialog(),
            Some(DialogKind::Connections | DialogKind::Models)
        ) || app.surfaces.surfaces.contains_sheet(SheetKind::ModelEditor)
            || app
                .surfaces
                .surfaces
                .contains_sheet(SheetKind::CustomProvider)
        {
            // These modals borrow the input line as their own field
            // (filter / key+model / history-query), so the composer
            // underneath would only duplicate the same `app.composer.input` the
            // modal already shows — and, since both are bound to the
            // one buffer, would read as a second live input field
            // accepting the same keystrokes. Its rect stays mounted
            // (so the footer layout is stable) but is left as recessed
            // surface — the dim pass darkens it like the rest of the
            // background. For the editor's key field the composer would
            // also panic: the masked key's byte cursor is computed
            // against the unmasked string.
        } else if !app.in_subagent_view() {
            // The composer stays mounted for the dim-recess modals
            // (Help / Session /
            // Activity) so the footer layout doesn't shift when the
            // overlay opens or closes; the recess pass darkens it in
            // place with the rest of the surface. When a transcript
            // pane holds the keyboard (Shift-Tab), the composer drops
            // to its dim "blurred" palette and hides the caret so the
            // user can see at a glance that the next keypress targets
            // the transcript. A composer-plane act (Shift-Tab back, a click into
            // the box) re-brightens it immediately.
            //
            // `show_caret` comes straight from the single source of
            // truth (`App::caret_visible`): in this branch the composer
            // is the only possible caret surface (the caret-owning
            // modals are handled by the `skip` branch above, and subagent
            // zoom is excluded by the `!in_subagent_view` gate), so
            // `caret_visible` reduces to "no step focus, no selection"
            // — exactly the old hand-rolled condition, without the risk
            // of drifting from the hide/show state machine.
            // Both transcript sub-states (a highlighted entry, browse
            // attention after a click) dim the composer panel, because both
            // mean the same thing to the keyboard: the transcript owns it
            // (ADR-0145).
            let active_extension = app.active_composer_extension();
            // One question, one answer: does the transcript plane own the
            // keyboard? Both its sub-states agree, so there is no separate
            // "step selected" and "browsing" pair to keep in sync here.
            let transcript_owns_keyboard = app.transcript.focus.is_transcript();
            let show_caret = scene_owns_caret && !transcript_owns_keyboard;
            let composer_focused = !transcript_owns_keyboard
                && (!has_overlay || scene_owns_caret || active_extension.is_some());
            // A fully-typed known `/command` is painted in bold +
            // accent color so it reads as a resolved command
            // rather than prose; an unmatched `/`-prefix keeps
            // the normal text color.
            let slash_len =
                resolved_slash_command_len(&app.composer.input, &app.composer.command_catalog);
            let byte_cursor = app.byte_cursor();
            let image_count = app.composer.pending_images.len();
            let paste_count = app.composer.pending_text_pastes.len();
            // Compose-target derivation happens while `&app` borrows are
            // still immutable; the result is an owned value the composer can
            // consume after taking its mutable borrows.
            let busy = app.session.running_sessions.contains(viewed_session_id);
            let composer_hints = {
                use crate::shell::composer_hints::{ComposerHints, compose_target_for_extension};
                ComposerHints {
                    compose_target: compose_target_for_extension(
                        busy,
                        Some(app.composer.composer_send_mode),
                        slash_len.is_some() || app.composer.input.starts_with('/'),
                        active_extension,
                        app.composer.history_index.is_some(),
                    ),
                    can_retry: !busy && viewed_chrome.can_retry,
                    history_recall: app.history_recall_badge(),
                    recall_draft_saved: !app.composer.draft_capsule.is_empty(),
                    toggle_mode_key: app
                        .surfaces
                        .surface_overrides
                        .effective_binding(crate::shell::keymap::SurfaceVerb::ToggleSendMode),
                    focused_entry: app.focused_entry_kind(),
                }
            };
            let history_ghost: Option<String> =
                if app.active_dialog() == Some(crate::shell::surfaces::DialogKind::HistorySearch) {
                    let ranked = app.history_rows();
                    let idx = app
                        .surfaces
                        .surfaces
                        .dlg::<crate::shell::surfaces::HistorySearchDialog>()
                        .index;
                    let pick = ranked.get(idx).or_else(|| ranked.first());
                    pick.and_then(|(orig_idx, _)| app.composer.input_history.get(*orig_idx))
                        .map(|e| e.text.clone())
                } else {
                    None
                };
            let composer_options = render::ComposerDrawOptions {
                focused: composer_focused,
                show_caret,
                follow_caret: app.composer.input_scroll_follow_cursor,
                record: true,
                image_count,
                paste_count,
                hints: composer_hints,
                ghost_text: history_ghost.as_deref(),
            };
            match slash_len {
                Some(len) => render::draw_composer_highlighted(
                    ComposerProps {
                        frame: f,
                        input_rect,
                        theme: &app.surfaces.theme,
                        layout_map,
                        input_scroll: &mut app.composer.input_scroll,
                        selection: &app.transcript.selection,
                    },
                    ComposerText {
                        input: &app.composer.input,
                        byte_cursor,
                    },
                    composer_options,
                    len,
                ),
                None if app.composer.input_scroll_follow_cursor => render::draw_composer(
                    ComposerProps {
                        frame: f,
                        input_rect,
                        theme: &app.surfaces.theme,
                        layout_map,
                        input_scroll: &mut app.composer.input_scroll,
                        selection: &app.transcript.selection,
                    },
                    ComposerText {
                        input: &app.composer.input,
                        byte_cursor,
                    },
                    composer_focused,
                    show_caret,
                    true,
                    image_count,
                    paste_count,
                    composer_hints,
                ),
                None => render::draw_composer_with_options(
                    ComposerProps {
                        frame: f,
                        input_rect,
                        theme: &app.surfaces.theme,
                        layout_map,
                        input_scroll: &mut app.composer.input_scroll,
                        selection: &app.transcript.selection,
                    },
                    ComposerText {
                        input: &app.composer.input,
                        byte_cursor,
                    },
                    composer_options,
                ),
            }
        }
    }
}

/// Persist the transcript pass's frame measurements back onto `app` for the
/// next iteration and click routing, and mount the sticky header target.
fn persist_transcript_layout(
    app: &mut App,
    ui: &mut crate::shell::ui::ComponentTree,
    transcript: &render::TranscriptRender,
) {
    app.transcript.content_lines = transcript.content_lines;
    app.transcript.view_height = transcript.view_height;
    app.transcript.max_scroll = transcript
        .content_lines
        .saturating_sub(transcript.view_height as usize)
        .min(u16::MAX as usize) as u16;
    // Hit-test rects for the footer bars, resolved from the one registry the
    // renderer placed this frame (`TranscriptRender::footer`) — one source
    // of truth instead of per-bar plumbing.
    // The composer panel's own rect, for the spatial mouse router (wheel
    // ticks and selection edge-autoscroll inside the box drive the input's
    // viewport, not the transcript). Zero-height / absent rows resolve to
    // `None` — a collapsed or hidden composer owns no pointer cell.
    match &transcript.sticky {
        Some(info) => {
            app.transcript.sticky_step = Some(info.message_idx);
            ui.mount(UiKey::Sticky, info.rect);
            app.transcript.sticky_summary_line = Some(info.summary_line);
        }
        None => {
            app.transcript.sticky_step = None;
            app.transcript.sticky_summary_line = None;
        }
    }
}

/// Interaction sheets family: the AI-initiated question and input-injection
/// sheets sit in the composer slot while the transcript stays live.
fn draw_interaction_sheets(
    app: &App,
    f: &mut nuotc::Frame<'_>,
    ui: &mut crate::shell::ui::ComponentTree,
    slot: ComposerSlot,
) {
    let input_rect = slot.input_rect;
    let hint_rect = slot.hint_rect;
    let is_fullscreen_scene = matches!(
        app.current_scene(),
        SceneKind::Dashboard | SceneKind::Settings | SceneKind::CommandLog
    );
    let chrome_hidden = is_fullscreen_scene;
    let overlay_owns_caret =
        app.caret_visible() && app.caret_owner() == crate::shell::CaretOwner::Overlay;
    // Interaction sheets: the AI-initiated sheets occupy the
    // composer slot — the same bottom edge, extended over the hint bar —
    // while the transcript behind them stays live. The question and
    // input-injection sheets paint their own panel over the composer's
    // slot; the permission sheet replaced the slot entirely above.
    if !chrome_hidden {
        match app.active_sheet() {
            Some(crate::shell::sheet::SheetKind::Question) => {
                if let Some(ref qmodel) = app.human.question {
                    let question_rect = nuotc::Rect::new(
                        input_rect.x,
                        input_rect.y,
                        input_rect.width,
                        input_rect.height
                            + nuo_ui_primitives::design::COMPOSER_HINT_GAP_ROWS
                            + hint_rect.height,
                    );
                    let props = crate::shell::overlays::ask_user::AskUserProps {
                        request: qmodel.request().clone(),
                        queue_depth: app.human.pending_question_depth,
                        follow_highlight: app.human.question_modal_follow,
                        show_caret: overlay_owns_caret,
                    };
                    let state = crate::shell::overlays::ask_user::AskUserState {
                        model: qmodel.clone(),
                        scroll: app.human.question_scroll,
                    };
                    crate::shell::overlays::ask_user::AskUser::render_with_tree(
                        &props,
                        &state,
                        f,
                        ui,
                        question_rect,
                        &app.surfaces.theme,
                    );
                }
            }
            Some(crate::shell::sheet::SheetKind::InputInjection) => {
                if let Some(ref req) = app.human.pending_input {
                    ui.mount(
                        UiKey::Sheet(crate::shell::sheet::SheetKind::InputInjection),
                        input_rect,
                    );
                    render::draw_input_injection(
                        f,
                        req,
                        &app.composer.input,
                        app.composer.cursor_position,
                        input_rect,
                        &app.surfaces.theme,
                    );
                }
            }
            _ => {}
        }
    }
}

/// Completion family: the slash-command / `@path` mention popup anchored to
/// the active trigger token.
fn draw_completion_popup(
    app: &mut App,
    f: &mut nuotc::Frame<'_>,
    mut ui: &mut crate::shell::ui::ComponentTree,
    layout_map: &mut LayoutMap,
    slot: ComposerSlot,
) {
    let input_rect = slot.input_rect;
    // Completion menu: slash commands or `@path` file mentions.
    // Honors `completion_dismissed` so Esc / Enter-commit keep the
    // popup hidden until the next edit clears the latch. Also
    // suppressed for a fully-typed command whose exact match is the
    // text already in the box — that is a *resolved* state (the
    // composer paints it bold + accent), the popup has nothing left
    // to offer, and ↑/↓ keep walking history instead of cycling a
    // single pinned row.
    if app.surfaces.surfaces.active_overlay().is_none()
        && app.active_sheet().is_none()
        && !app.composer.completion_dismissed
        && app.completion_kind() != CompletionKind::None
    {
        let completions = app.completions();
        // Anchor pass (the frame-side twin of the event loop's pre-compute):
        // any state change that bypassed a keystroke — a paste, an async
        // project-scan landing, a modal teardown — still lands here, so this
        // is the last line of defense keeping "popup visible ⇒ one row
        // highlighted" true. A freshly opened menu starts at its first
        // candidate; a stale index clamps; nothing visible clears.
        app.anchor_completion_selection(&completions);
        let exact_match = completions.iter().any(|c| {
            c.replace_start == 0
                && c.replace_end == app.composer.input.len()
                && c.label == app.composer.input
        });
        if !completions.is_empty() && !exact_match {
            // Anchor the popup relative to the active trigger token
            // (e.g. the `@` in a mention or `/` command) in both X and Y,
            // tracking the cursor's wrapped row and scroll offset so the menu
            // hovers immediately above the line being typed.
            let anchor = completion_anchor(
                &app.composer.input,
                app.byte_cursor(),
                input_rect,
                app.composer.input_scroll,
                app.completion_kind(),
            );
            render::draw_completion_menu(
                f,
                layout_map,
                Some(&mut ui),
                &completions,
                app.composer.suggestion_index,
                anchor,
                &app.surfaces.theme,
            );
        }
    }
}

/// Recess the live surface and dispatch to the active overlay or the
/// full-screen scene. The recess is a read-modify-write of cells already on
/// the grid, so it paints straight into the frame.
fn draw_recessed_surface(
    app: &mut App,
    f: &mut nuotc::Frame<'_>,
    viewed_session_id: &str,
    ui: &mut crate::shell::ui::ComponentTree,
    layout_map: &mut LayoutMap,
    transcript: &render::TranscriptRender,
) -> SurfaceDraw {
    let is_fullscreen_scene = matches!(
        app.current_scene(),
        SceneKind::Dashboard | SceneKind::Settings | SceneKind::CommandLog
    );
    let has_overlay = app.surfaces.surfaces.active_overlay().is_some();
    let recess = if is_fullscreen_scene {
        Recess::Takeover
    } else if app.surfaces.surfaces.active_overlay()
        == Some(OverlaySurface::Dialog(DialogKind::HistorySearch))
    {
        Recess::None
    } else if has_overlay {
        Recess::Dim
    } else {
        Recess::None
    };
    ui.mount(UiKey::Backdrop, f.area());
    render::recess_backdrop(f, recess, &app.surfaces.theme);

    if let Some(overlay) = app.surfaces.surfaces.active_overlay() {
        SurfaceDraw {
            modal_rect: draw_active_overlay(
                app,
                f,
                viewed_session_id,
                ui,
                layout_map,
                overlay,
                transcript,
            ),
            dashboard_list_body_height: None,
        }
    } else {
        draw_scene(app, f, viewed_session_id, ui, layout_map)
    }
}

/// Overlay family: dialogs (the `?` keys view or the entity's own view) and
/// provider sheets. Returns the panel rect the renderer painted, used for
/// modal-body sizing and backdrop hit-testing.
fn draw_active_overlay(
    app: &mut App,
    f: &mut nuotc::Frame<'_>,
    viewed_session_id: &str,
    mut ui: &mut crate::shell::ui::ComponentTree,
    layout_map: &mut LayoutMap,
    overlay: OverlaySurface,
    transcript: &render::TranscriptRender,
) -> Option<nuotc::Rect> {
    let viewed_running = app.session.running_sessions.contains(viewed_session_id);
    let overlay_owns_caret =
        app.caret_visible() && app.caret_owner() == crate::shell::CaretOwner::Overlay;
    let spinner_phase = (app.transcript.spinner_epoch.elapsed().as_millis() / 100) as usize;
    let input_rect = transcript.input_rect;
    let activity_height =
        render::footer_rect(&transcript.footer, render::FooterRowId::ActivityStrip)
            .map_or(0, |r| r.height);

    match overlay {
        OverlaySurface::Dialog(d) if app.dialog_keys() => {
            let app_ctx = crate::shell::keymap::AppContext {
                has_overlay: true,
                active_dialog: Some(d),
                is_responding: viewed_running,
                has_selection: !matches!(
                    app.transcript.selection,
                    nuo_view::model::selection::SelectionState::None
                ),
                has_running_task: viewed_running,
                has_notices: !!app.has_notices(),
                has_focused_entry: app.transcript.focus.entry().is_some(),
                queue_count: app.outbox_total(),
                has_session: app.has_session(),
                scene: app.current_scene(),
            };
            Some(render::draw_dialog_keys(
                f,
                d,
                app.surfaces.surfaces.dialogs.keys_scroll_mut(d),
                &app_ctx,
                &app.surfaces.theme,
                &app.transcript.selection,
                layout_map,
            ))
        }
        OverlaySurface::Dialog(_) => {
            // Route through the encapsulated entity's `DialogView::render`
            // (ADR-0035 §1). The entity is taken out of its stack entry so
            // it can render against a disjoint `&App` borrow, then put back.
            if let Some(mut ent) = app.surfaces.surfaces.take_active_view() {
                let startup_picker =
                    app.surfaces.startup_overlay == crate::shell::StartupOverlay::SessionsPicker;
                let rect = {
                    let mut ctx = crate::shell::surfaces::DialogRenderCtx {
                        app,
                        layout_map,
                        selection: &app.transcript.selection,
                        theme: &app.surfaces.theme,
                        spinner_phase,
                        viewed_session_id,
                        startup_picker,
                        input_rect: Some(input_rect),
                        activity_height,
                        overlay_owns_caret,
                    };
                    ent.render(f, f.area(), &mut ctx)
                };
                app.surfaces.surfaces.put_active_view(ent);
                rect
            } else {
                None
            }
        }
        OverlaySurface::Sheet(s) => match s {
            SheetKind::ModelEditor => {
                if let Some(target) = app.provider_editor.editor_target.as_deref()
                    && (target.starts_with("web_credential:")
                        || target.starts_with("web_endpoint:"))
                {
                    let endpoint = target.starts_with("web_endpoint:");
                    Some(render::draw_web_value_editor(
                        f,
                        &format!("Configure {}", app.provider_editor.editor_model),
                        if endpoint { "Endpoint" } else { "API token" },
                        &app.composer.input,
                        app.composer.cursor_position,
                        !endpoint,
                        overlay_owns_caret,
                        &app.surfaces.theme,
                    ))
                } else {
                    let title = if app.provider_editor.editor_model_settings_only {
                        app.provider_editor.editor_model.clone()
                    } else {
                        app.provider_editor
                            .editor_target
                            .as_deref()
                            .and_then(|id| {
                                app.surfaces
                                    .provider_picker
                                    .rows
                                    .iter()
                                    .find(|r| r.id == id)
                            })
                            .map(|r| r.name.clone())
                            .unwrap_or_else(|| "model".to_string())
                    };
                    let effort = app
                        .provider_editor
                        .editor_model_settings_only
                        .then_some(app.provider_editor.editor_effort.as_str());
                    // The ladder captured from the snapshot when the editor
                    // opened. Deliberately NOT re-derived with
                    // `resolve_model`: this binary does not link
                    // `nuo-providers`, so the provider baseline tables are
                    // absent and a client-side resolve returns an empty
                    // ladder — which collapsed the node slider to the
                    // value-only row. Empty stays empty (a route with no
                    // effort knob keeps the value-only fallback).
                    let effort_levels: Vec<String> =
                        if app.provider_editor.editor_model_settings_only {
                            app.provider_editor.editor_effort_levels.clone()
                        } else {
                            Vec::new()
                        };
                    let thinking = app
                        .provider_editor
                        .editor_model_settings_only
                        .then_some(app.provider_editor.editor_thinking)
                        .filter(|_| app.provider_editor.editor_thinking_available);
                    let overrides = app.provider_editor.editor_model_settings_only.then_some((
                        app.provider_editor.editor_vision_override,
                        app.provider_editor.editor_tool_override,
                    ));
                    Some(render::draw_model_editor(
                        f,
                        &title,
                        &app.composer.input,
                        app.composer.cursor_position,
                        !app.provider_editor.editor_model_settings_only,
                        app.provider_editor.editor_field,
                        overlay_owns_caret,
                        effort,
                        &effort_levels,
                        thinking,
                        overrides,
                        &app.surfaces.theme,
                    ))
                }
            }
            SheetKind::ProviderPreset => Some(render::draw_preset_chooser(
                app.provider_editor.preset_choice,
                f,
                &app.surfaces.theme,
                &mut app.provider_editor.preset_scroll,
            )),
            SheetKind::OAuthPending => {
                let title: &'static str = match &app.provider_editor.custom_auth {
                    nuo_protocol::ConnectionAuth::Negotiated { provider } => {
                        match provider.as_ref() {
                            "chatgpt" => "ChatGPT Subscription",
                            "copilot" => "Copilot",
                            "xai" => "xAI",
                            "google-antigravity" => "Google Antigravity",
                            "qoder" => "Qoder",
                            _ => "Subscription",
                        }
                    }
                    nuo_protocol::ConnectionAuth::Static => "OAuth",
                };
                Some(render::draw_oauth_pending(
                    title,
                    &app.provider_editor.oauth_pending_message,
                    &app.provider_editor.oauth_pending_url,
                    &app.provider_editor.oauth_pending_user_code,
                    app.provider_editor.oauth_pending_error.as_deref(),
                    app.provider_editor.oauth_selected_item,
                    f,
                    &app.surfaces.theme,
                    &mut app.provider_editor.oauth_scroll,
                    Some(&mut ui),
                    &app.transcript.selection,
                    layout_map,
                ))
            }
            SheetKind::CustomProvider => {
                let editing = app.custom_is_editing();
                let title = if editing {
                    format!("Edit — {}", app.provider_editor.custom_name)
                } else {
                    crate::provider_label_for(app.provider_editor.custom_provider_id.as_deref())
                };
                let protocol_display = app
                    .provider_editor
                    .custom_protocol_wire
                    .parse::<nuo_protocol::WireProtocol>()
                    .map(|p| p.display_name())
                    .unwrap_or(&app.provider_editor.custom_protocol_wire);
                let budget_display = match &app.provider_editor.custom_budget {
                    nuo_protocol::ConnectionBudget::Remote => "Remote (Upstream)".to_string(),
                    nuo_protocol::ConnectionBudget::Local { limit, period, .. } => {
                        format!("Local ({}, {})", limit.format_usd(), period.label())
                    }
                };
                Some(render::draw_custom_provider_editor(
                    render::CustomEditorProps {
                        fields: &app.provider_editor.custom_fields,
                        field: app.provider_editor.custom_field,
                        editing,
                        custom: app.provider_editor.custom_provider_id.as_deref()
                            == Some(nuo_view::providers::CUSTOM_TEMPLATE.id),
                        title: &title,
                        name_buf: &app.provider_editor.custom_name,
                        base_url_buf: &app.provider_editor.custom_base_url,
                        token_buf: &app.provider_editor.custom_token,
                        model_buf: &app.provider_editor.custom_model,
                        protocol_display,
                        budget_display: &budget_display,
                        url_hint: &app.provider_editor.custom_url_hint,
                        input: &app.composer.input,
                        cursor_position: app.composer.cursor_position,
                    },
                    f,
                    &app.surfaces.theme,
                    &mut app.provider_editor.custom_scroll,
                    overlay_owns_caret,
                ))
            }
            _ => None,
        },
    }
}

/// Full-screen scene family: dashboard/host list, settings, and the command
/// log. The dashboard additionally reports its true list-body height.
fn draw_scene(
    app: &mut App,
    f: &mut nuotc::Frame<'_>,
    viewed_session_id: &str,
    ui: &mut crate::shell::ui::ComponentTree,
    layout_map: &mut LayoutMap,
) -> SurfaceDraw {
    let scene_unread_faults = if app.has_unread_notice_fault() {
        app.unread_notices()
    } else {
        0
    };
    let scene_prompt_owns_caret =
        app.caret_visible() && app.caret_owner() == crate::shell::CaretOwner::Scene;
    let mut dashboard_list_body_height: Option<u16> = None;
    let modal_rect = match app.current_scene() {
        SceneKind::Dashboard => {
            let rects = render::draw_dashboard(
                f,
                crate::shell::overlays::dashboard::DashboardProps {
                    rows: &app.session.host_sessions,
                    selected: app
                        .surfaces
                        .modal_index
                        .min(app.session.host_sessions.len().saturating_sub(1)),
                    focus: app.surfaces.host_focus,
                    list_scroll: &mut app.surfaces.host_scroll,
                    list_follow: app.surfaces.host_modal_follow,
                    detail_scroll: &mut app.surfaces.host_detail_scroll,
                    log: &app.transcript.host_console_log,
                    prompting: app.surfaces.host_prompting,
                    prompt_create_new: app.surfaces.host_prompt_new,
                    prompt_text: &app.composer.input,
                    cursor_position: app.composer.cursor_position,
                    current_session_id: viewed_session_id,
                    show_caret: scene_prompt_owns_caret,
                    tab_bar: Some(render::TabBarProps {
                        tabs: Some(app.surfaces.surfaces.tabs()),
                        active_tab: app.surfaces.surfaces.active_tab_index(),
                        unread_faults: scene_unread_faults,
                    }),
                    filter: &app.surfaces.dashboard_filter,
                    filtering: app.surfaces.dashboard_filtering,
                    view_usage: app.surfaces.dashboard_view_usage,
                    usage_report: app.telemetry.usage_stats.as_ref(),
                    usage_loading: app.telemetry.usage_loading,
                    usage_scroll: app.telemetry.usage_scroll,
                },
                &app.surfaces.theme,
            );
            dashboard_list_body_height = Some(rects.list_body.height);
            if let Some(rect) = rects.tab_bar_rect {
                ui.mount(UiKey::TabBar, rect);
            }
            for (idx, tab_rect) in &rects.tab_rects {
                ui.mount(UiKey::TabBarItem(*idx), *tab_rect);
            }
            if let Some(rect) = rects.tab_prev_rect {
                ui.mount(UiKey::TabBarPrev, rect);
            }
            if let Some(rect) = rects.tab_next_rect {
                ui.mount(UiKey::TabBarNext, rect);
            }
            if let Some(rect) = rects.tab_overflow_rect {
                ui.mount(UiKey::TabBarOverview, rect);
            }
            if let Some(rect) = rects.tab_notices_rect {
                ui.mount(UiKey::NoticesBadge, rect);
            }
            if let Some(rect) = rects.tab_help_rect {
                ui.mount(UiKey::HelpBadge, rect);
            }
            if let Some(preview_id) = &app.surfaces.host_preview {
                let row = app
                    .session
                    .host_sessions
                    .iter()
                    .find(|r| &r.id == preview_id);
                render::draw_session_preview(
                    f,
                    row,
                    &mut app.surfaces.host_preview_scroll,
                    &app.surfaces.theme,
                );
            }
            Some(rects.area)
        }
        SceneKind::Settings => {
            let breadcrumbs_str = if app.in_subagent_view() {
                "Main › Subagent › Settings"
            } else {
                "Main › Settings"
            };
            let rects = render::draw_settings_view(
                f,
                render::SettingsProps {
                    category_index: app.surfaces.config_category,
                    detail_index: app.surfaces.config_detail_index,
                    hover_index: app.surfaces.config_hover_index,
                    focus: app.surfaces.config_focus,
                    color_scheme: &app.surfaces.color_scheme,
                    custom_color_scheme: &app.surfaces.custom_color_scheme,
                    websearch: app.surfaces.websearch_config.as_ref(),
                    workspace: &app.session.current_workspace,
                    category_scroll: &mut app.surfaces.config_scroll,
                    detail_scroll: &mut app.surfaces.config_detail_scroll,
                    breadcrumbs: Some(breadcrumbs_str),
                    theme: &app.surfaces.theme,
                    profile: &app.surfaces.profile,
                    tui_config: &app.session.tui_config,
                    tab_bar: Some(render::TabBarProps {
                        tabs: Some(app.surfaces.surfaces.tabs()),
                        active_tab: app.surfaces.surfaces.active_tab_index(),
                        unread_faults: scene_unread_faults,
                    }),
                },
            );
            app.surfaces.config_selected_rect = rects.selected_row_rect;
            if let Some(rect) = rects.tab_bar_rect {
                ui.mount(UiKey::TabBar, rect);
            }
            for (idx, tab_rect) in &rects.tab_rects {
                ui.mount(UiKey::TabBarItem(*idx), *tab_rect);
            }
            if let Some(rect) = rects.tab_prev_rect {
                ui.mount(UiKey::TabBarPrev, rect);
            }
            if let Some(rect) = rects.tab_next_rect {
                ui.mount(UiKey::TabBarNext, rect);
            }
            if let Some(rect) = rects.tab_overflow_rect {
                ui.mount(UiKey::TabBarOverview, rect);
            }
            if let Some(rect) = rects.tab_notices_rect {
                ui.mount(UiKey::NoticesBadge, rect);
            }
            if let Some(rect) = rects.tab_help_rect {
                ui.mount(UiKey::HelpBadge, rect);
            }
            for (index, cat_rect) in rects.category_rects {
                ui.mount(UiKey::SettingsCategory(index), cat_rect);
            }
            if let Some(row_rect) = rects.selected_row_rect {
                ui.mount(
                    UiKey::SettingsOption(app.surfaces.config_detail_index),
                    row_rect,
                );
            }
            // Full-block pointer targets for every visible row, so the row
            // under the mouse (label *or* description line) lights up. Mounted
            // after the cursor's own target so the taller block wins the hit
            // test; both resolve to the same row index, so the choice is moot.
            for (index, row_rect) in rects.row_rects {
                ui.mount(UiKey::SettingsRow(index), row_rect);
            }
            if let Some((ref mut state, ref mut anchor)) = app.surfaces.config_dropdown {
                if let Some(target_rect) = app.surfaces.config_selected_rect
                    && anchor.placement
                        != nuo_ui_primitives::components::dropdown::DropdownPlacement::CenterScreen
                {
                    anchor.target_rect = target_rect;
                }
                let popup_area = nuo_ui_primitives::components::dropdown::draw_dropdown(
                    f,
                    state,
                    anchor,
                    &app.surfaces.theme,
                    f.area(),
                );
                ui.mount(UiKey::ConfigDropdown, popup_area);
            }
            Some(rects.area)
        }
        SceneKind::CommandLog => {
            // The log reads one step's payload; a log whose step vanished
            // (transcript rebuild) is a stated empty state, not a panic and
            // not a silently blank page.
            let rects = render::draw_command_log_scene(
                f,
                render::LogSceneProps {
                    log: app.transcript.command_log.as_ref(),
                    scroll: &mut app.surfaces.command_log_scroll,
                    thread_id: Some(viewed_session_id),
                    tab_bar: Some(render::TabBarProps {
                        tabs: Some(app.surfaces.surfaces.tabs()),
                        active_tab: app.surfaces.surfaces.active_tab_index(),
                        unread_faults: scene_unread_faults,
                    }),
                },
                layout_map,
                &app.surfaces.theme,
            );
            if let Some(rect) = rects.tab_bar_rect {
                ui.mount(UiKey::TabBar, rect);
            }
            for (idx, tab_rect) in &rects.tab_rects {
                ui.mount(UiKey::TabBarItem(*idx), *tab_rect);
            }
            if let Some(rect) = rects.tab_prev_rect {
                ui.mount(UiKey::TabBarPrev, rect);
            }
            if let Some(rect) = rects.tab_next_rect {
                ui.mount(UiKey::TabBarNext, rect);
            }
            if let Some(rect) = rects.tab_overflow_rect {
                ui.mount(UiKey::TabBarOverview, rect);
            }
            if let Some(rect) = rects.tab_notices_rect {
                ui.mount(UiKey::NoticesBadge, rect);
            }
            if let Some(rect) = rects.tab_help_rect {
                ui.mount(UiKey::HelpBadge, rect);
            }
            // The dashboard's list-body slot stays unset: this scene's scroll
            // is its own field and its page step is derived from the body
            // rect the renderer just clamped against.
            Some(rects.area)
        }
        SceneKind::Thread | SceneKind::Subagent => None,
    };

    SurfaceDraw {
        modal_rect,
        dashboard_list_body_height,
    }
}

/// Provider-delete confirmation family: painted on top of the Connections
/// list only while a deletion is staged from `Shift+D`.
fn draw_provider_delete_confirm(
    app: &App,
    f: &mut nuotc::Frame<'_>,
    ui: &mut crate::shell::ui::ComponentTree,
) {
    if app.active_dialog() == Some(DialogKind::Connections)
        && let Some(ref pending_id) = app.surfaces.pending_provider_delete
    {
        let provider_name = app
            .surfaces
            .provider_picker
            .rows
            .iter()
            .find(|r| &r.id == pending_id)
            .map(|r| r.name.clone())
            .unwrap_or_else(|| pending_id.clone());
        let rect = render::draw_provider_delete_confirm(
            f,
            &provider_name,
            match app.surfaces.provider_delete_focus {
                ProviderDeleteChoice::Cancel => ConfirmChoice::Cancel,
                ProviderDeleteChoice::Delete => ConfirmChoice::Delete,
            },
            &app.surfaces.theme,
        );
        ui.mount(UiKey::ProviderDelete, rect);
    }
}

/// Toast family: urgent confirmation toasts take precedence over
/// informational toasts, since they share one top-right slot.
fn draw_toasts(app: &App, f: &mut nuotc::Frame<'_>) {
    if app.esc_armed() {
        render::draw_armed_toast(f, "Esc again interrupts", &app.surfaces.theme);
    } else if app.ctrl_c_armed() {
        render::draw_armed_toast(f, "press Ctrl-c again to detach", &app.surfaces.theme);
    } else if app.surfaces.copy_toast_until.is_some() {
        render::draw_copy_toast(
            f,
            &app.surfaces.copy_toast_message,
            app.surfaces.copy_toast_failed,
            &app.surfaces.theme,
        );
    } else if app.surfaces.notice_toast_until.is_some() {
        // A fault bubble, or a command acknowledgment. Rendered only when no
        // higher-priority toast is showing, since they share the same top-right
        // slot. The hint row names the viewer: a bubble is a pointer to the
        // record, never the record itself.
        let hint = app.notice_bubble_hint();
        render::draw_notice_toast(
            f,
            &app.surfaces.notice_toast_message,
            app.surfaces.notice_toast_severity,
            &app.surfaces.theme,
            hint.as_deref(),
        );
    }
}

/// Frame tail: stage the hit-test document, record the open modal's body
/// height for page-step sizing, and mount the painted panel rect so a click
/// on the backdrop outside it can dismiss.
fn finish_frame(
    app: &mut App,
    ui: &mut crate::shell::ui::ComponentTree,
    layout_map: &mut LayoutMap,
    surface: SurfaceDraw,
) {
    ui.stage_document(std::mem::take(layout_map));

    // Capture the open modal's body height for page-scroll step
    // sizing. The renderer returns the full panel rect; the body is
    // that rect minus the header/footer/padding chrome. All
    // centered modals that paint a scrollable body use the same
    // `modal_frame(header, footer)` chrome, so the row count is the
    // shared `modal_chrome_rows` for a header+footer spec. Stays 0
    // for modals that return no rect (Permission sheet, which
    // scrolls the transcript behind it via `view_height` instead),
    // so the page step falls back to the transcript height there.
    app.transcript.modal_body_height = match surface.dashboard_list_body_height {
        // The dashboard's scroll body is its list pane, whose height
        // was reported directly by the renderer.
        Some(h) => h,
        None => surface
            .modal_rect
            .map(|r| {
                r.height
                    .saturating_sub(nuo_ui_primitives::primitives::modal_chrome_rows(
                        nuo_ui_primitives::primitives::ModalSpec {
                            width_percent: 0,
                            header: true,
                            footer: true,
                        },
                    ))
            })
            .unwrap_or(0),
    };

    // Record the open modal's actual panel rect (when one is
    // dismissable) so a click on the backdrop outside it can close it.
    // The rect comes from the renderer that just painted the panel, so
    // dynamic-height modals and click hit-tests cannot drift apart.
    if let (Some(rect), Some(overlay)) =
        (surface.modal_rect, app.surfaces.surfaces.active_overlay())
    {
        ui.mount(UiKey::Overlay(overlay), rect);
    }
}

/// [INV-MCP-04]: Extract in-flight MCP connection status during system bootstrapping.
fn mcp_connecting_status(app: &App) -> Option<String> {
    let snapshot = app.session.session_context.as_ref()?;
    if snapshot.mcp.is_empty() {
        return None;
    }
    let connecting: Vec<&str> = snapshot
        .mcp
        .iter()
        .filter(|s| !s.connected && !s.disabled && s.failure.is_none())
        .map(|s| s.name.as_str())
        .collect();
    if connecting.is_empty() {
        return None;
    }
    let total = snapshot.mcp.iter().filter(|s| !s.disabled).count();
    let connected = snapshot.mcp.iter().filter(|s| s.connected).count();
    let names = connecting.join(", ");
    Some(format!("connecting MCP ({connected}/{total}: {names})…"))
}
