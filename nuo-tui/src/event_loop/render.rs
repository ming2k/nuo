//! The per-frame draw routine for the TUI event loop, extracted from
//! `run_app_loop`'s `if needs_draw` stage (it was a ~1000-line closure).

use crate::completion::{CompletionKind, completion_anchor, resolved_slash_command_len};
use crate::composer::{ComposerProps, ComposerText};
use crate::model::document::TranscriptMessage;
use crate::model::layout::LayoutMap;
use crate::overlays::provider_delete_confirm::ProviderDeleteChoice as ConfirmChoice;
use crate::primitives::Recess;
use crate::render;
use crate::surfaces::{DialogKind, OverlaySurface, SceneKind, SheetKind};
use crate::ui::UiKey;
use crate::{App, ProviderDeleteChoice};

use super::actions::effective_reasoning_effort;
use super::transcript::display_status;

/// Loop stage: the per-frame draw. Paints the chrome (startup picker,
/// transcript, hint bar, composer, completion popup), recesses the live
/// surface for the open modal, then draws the
/// active modal panel, persisting per-frame layout state back onto `app`.
/// Invoked through `Terminal::stage` (bottom-follow measurement pass) or
/// `Terminal::draw`; extracted verbatim from the `render_frame` closure.
pub(crate) fn render_frame(app: &mut App, f: &mut nuotc::Frame<'_>, viewed_session_id: &str) {
    let mut ui = std::mem::take(&mut app.ui);
    compose_frame(app, f, viewed_session_id, &mut ui);
    app.ui = ui;
}

fn compose_frame(
    app: &mut App,
    f: &mut nuotc::Frame<'_>,
    viewed_session_id: &str,
    mut ui: &mut crate::ui::ComponentTree,
) {
    ui.begin(f.area());
    // Modality is scene-owned (ADR-0197 §D2): a modal owns keys because the
    // mounted component declares `Modal` policy, not because an app flag says
    // so. The mount here pre-registers the modal at viewport size; the modal
    // renderer places it at its drawn rect further down.
    if let Some(overlay) = app.surfaces.active_overlay() {
        ui.mount(UiKey::Overlay(overlay), f.area());
    }
    if app.config_dropdown.is_some() {
        ui.mount(UiKey::ConfigDropdown, f.area());
    }
    let mut layout_map = LayoutMap::new();

    // ADR-0175: PreAttach interstitial takes over the terminal before
    // any chat/composer/transcript chrome is painted. The render
    // early-returns, exactly like the SessionsPicker guard below, so
    // nothing downstream (chrome, composer, panels) runs while the
    // workspace trust decision is still pending. The only interactive
    // surface on this frame is the trust prompt rendered by
    // `draw_pre_attach`.
    if let Some(pre_attach_state) = app.pre_attach.as_ref() {
        crate::pre_attach::draw_pre_attach(f, pre_attach_state, &app.theme);
        ui.mount(crate::ui::UiKey::PreAttach, f.area());
        // Layout map / modal rect / modal hit map stay empty — there
        // is no chrome to interact with, and the PreAttach surface
        // owns its own hit-testing (it does not consume LayoutMap).
        ui.stage_document(layout_map);
        return;
    }

    if app.startup_overlay == crate::StartupOverlay::SessionsPicker
        && app.active_dialog() == Some(DialogKind::Sessions)
    {
        // `nuo attach` (no id): initial launch opens ONLY the sessions picker
        // on a clean background. Do not open/render the chat interface, empty state,
        // composer input box, status bar, or header components until a session is selected.
        f.render_widget(
            nuotc::widgets::Block::default()
                .style(nuotc::Style::default().bg(app.theme.app_bg)),
            f.area(),
        );

        let spinner_phase = (app.spinner_epoch.elapsed().as_millis() / 100) as usize;
        let projected_count = crate::overlays::session::project_session_rows(
            &app.sessions_overview,
            Some(&app.surfaces.dlg::<crate::surfaces::SessionsDialog>().expanded),
        )
        .len();
        let startup_picker = app.startup_overlay == crate::StartupOverlay::SessionsPicker;
        let sessions = app.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>();
        let drawn_modal_rect = render::draw_sessions_modal(
            f,
            crate::overlays::session::SessionsModalProps {
                sessions: &app.sessions_overview,
                expanded_sessions: Some(&sessions.expanded),
                selected: sessions.index.min(projected_count.saturating_sub(1)),
                scroll: &mut sessions.scroll,
                follow: sessions.follow,
                startup_picker,
                spinner_phase,
                session_info_detail: sessions.info_detail,
                session_detail: app.session_detail.as_ref(),
                session_info_scroll: &mut sessions.info_scroll,
                sessions_loading: sessions.loading,
            },
            &app.theme,
            &app.selection,
            &mut layout_map,
        );

        ui.stage_document(layout_map);
        app.modal_body_height =
            drawn_modal_rect
                .height
                .saturating_sub(crate::primitives::modal_chrome_rows(
                    crate::primitives::ModalSpec {
                        width_percent: 0,
                        header: true,
                        footer: true,
                    },
                ));
        if let Some(overlay) = app.surfaces.active_overlay() {
            ui.mount(UiKey::Overlay(overlay), drawn_modal_rect);
        }
        return;
    }

    // Borrow the height cache out of `app` for the duration of the draw:
    // `view_messages` borrows `app` immutably below, so the cache cannot
    // also be reached through `app` at the same time. It is restored once
    // `view_messages` is no longer borrowed (see below).
    let mut height_cache = std::mem::take(&mut app.layout_height_cache);
    // View-scoped chrome: render the phase of whichever session the user is
    // viewing — the focused aside's own entry inside `/btw`, the primary's
    // otherwise. This is the aside-view activity-bar fix: the displayed bar
    // tracks the *viewed* session, never a global blend. A pending permission
    // sheet overrides the phase with its gate label (warning hue downstream).
    let viewed_chrome = app.viewed_chrome();
    let gate_phase = app
        .pending_permission
        .is_some()
        .then_some(crate::phase::Phase::AwaitingUser);
    let status = if let Some(ref target) = app.switching_session {
        format!("loading session {target}…")
    } else if app.link_down {
        // Dead-link chrome state (ADR-0197 D6): the server link is gone, so
        // user intents cannot be delivered. The live-status bar is the
        // visible anchor; it stays until the process exits, because nothing
        // can acknowledge recovery.
        "server link lost".to_string()
    } else {
        let base_status = display_status(
            app.loop_status,
            gate_phase.as_ref().or(viewed_chrome.phase.as_ref()),
        );
        // ADR-0240: Prioritize system lifecycle (e.g. MCP connecting progress) when round is idle.
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
    // Read from the *viewed* session's chrome (ADR-0235): the clause annotates
    // that session's phase, so a background aside backing off against a
    // rate-limited upstream cannot paint a countdown onto the primary's bar.
    // Its lifetime is already over if the phase moved on — `set_phase` retires
    // it — so this needs no staleness check of its own.
    let now = std::time::Instant::now();
    let backoff_clause = viewed_chrome
        .transport_setback
        .as_ref()
        .map(|setback| setback.summary(now));

    // Compute the displayed input text first so the transcript layout can
    // reserve the right height for a wrapping, growing input box.
    //
    // The text and the caret's byte offset are masked *as a pair*: the view
    // treats `byte_cursor` as an offset into `input` (the wrap engine maps
    // it onto the wrapped grid, the height reservation synthesizes a
    // trailing caret row from it). Pairing a masked string with the
    // unmasked offset happened to work only because `•` is 3 bytes per
    // char while the underlying text is 1–4 — an invariant nobody
    // guarantees. Mapping the char-indexed caret through the same mask
    // keeps every consumer operating on one coherent string.
    let (masked_input, masked_byte_cursor) =
        if app.surfaces.contains_sheet(SheetKind::ModelEditor) && app.editor_field == 0 {
            // Mask the API key everywhere it could be rendered (the editor
            // field itself, and any layout pass that inspects the input).
            let mask = "•".repeat(app.input.chars().count());
            // Byte offset of the caret inside the masked string: each char
            // maps to exactly one `•`, so the caret's char index maps to
            // `index * mask_char_len`.
            const MASK_CHAR: &str = "•";
            let caret_byte = MASK_CHAR.len() * app.cursor_position.min(mask.chars().count());
            (mask, caret_byte)
        } else {
            (app.input.clone(), app.byte_cursor())
        };

    let is_fullscreen_scene = matches!(
        app.current_scene(),
        SceneKind::Dashboard | SceneKind::Settings
    );
    let has_overlay = app.surfaces.active_overlay().is_some();
    let chrome_hidden = is_fullscreen_scene;
    let recess = if is_fullscreen_scene {
        Recess::Takeover
    } else if app.surfaces.active_overlay()
        == Some(OverlaySurface::Dialog(DialogKind::HistorySearch))
    {
        Recess::None
    } else if has_overlay {
        Recess::Dim
    } else {
        Recess::None
    };

    // The frame-level caret arbitration (ADR-0205). Exactly one layer owns the
    // physical terminal cursor; every overlay renderer receives this single
    // verdict (`App::caret_visible` + `App::caret_owner`) instead of
    // re-deriving it from its own local flags — which is how a suspended
    // surface could previously park the cursor inside itself while a different
    // layer was the keyboard foreground.
    let overlay_owns_caret = app.caret_visible() && app.caret_owner() == crate::CaretOwner::Overlay;
    let scene_owns_caret = app.caret_visible() && app.caret_owner() == crate::CaretOwner::Composer;
    // A non-conversation scene's own inline prompt (the Dashboard task line)
    // owns the cursor through its scene chrome rather than through SceneKeys.
    let scene_prompt_owns_caret =
        app.caret_visible() && app.caret_owner() == crate::CaretOwner::Scene;
    // A scene hint row (head legend, model-bar keycaps) advertises chords that
    // are suspended the moment an overlay takes the keyboard foreground, so it
    // is suppressed rather than left dimmed-but-readable behind the backdrop.
    let scene_chrome_hints = !has_overlay;

    // When zoomed into a Subagent, render its child messages and
    // show a contextual first-row header; otherwise render the
    // root conversation.
    let view_messages = app.focused_messages();
    // `/btw` aside scene context (ADR-0017, ADR-0024): shown only while
    // the aside view is active. Subagent zoom and the aside view are mutually
    // exclusive, so the two modes never coexist.
    let side_banner = app.in_side_view.then_some(app.parent_status);
    // The viewed session's run state, consumed by the overlay/palette
    // availability filters. The Subagent scene's own activity advertising is
    // suppressed entirely (`in_subagent` below), and its Esc interrupt is
    // resolved scene-scoped in `session::resolve_subagent_key`, so this stays
    // the *viewed session's* state — an overlay opened over the zoom still
    // reflects the (primary) round a palette `Interrupt Task` would stop.
    let viewed_running = app.running_sessions.contains(viewed_session_id);
    let subagent_bar = app.focus_stack.last().and_then(|current| {
        let tasks: Vec<&TranscriptMessage> = app
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
    // The scene row (ADR-0024): the scene the user stands in, named plainly,
    // followed by the scene's own context. Subagent outranks the aside view in
    // this resolution (they are mutually exclusive in the app; keeping a
    // deterministic precedence guards a malformed caller).
    let (scene_kind, scene_context, scene_context_warn): (render::ViewKind, Option<String>, bool) =
        if let Some(bar) = subagent_bar.as_ref() {
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
            (render::ViewKind::Subagent, Some(context), false)
        } else if let Some(parent) = side_banner {
            (
                render::ViewKind::Btw,
                Some(render::parent_status_context(parent).to_string()),
                render::parent_status_needs_attention(parent),
            )
        } else {
            (
                render::ViewKind::Session,
                conversation_title(app.focused_messages()),
                false,
            )
        };
    let page_hints = render::ViewHints {
        kind: scene_kind,
        context: scene_context.as_deref(),
        context_warn: scene_context_warn,
        unattended: app.unattended,
        confined: app.confined,
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
    let has_keyed_provider = app
        .provider_picker
        .rows
        .iter()
        .any(|row| row.key_ready || app.key_status.get(&row.id).copied().unwrap_or(true))
        || app.provider_picker.rows.is_empty();
    let guidance = if let Some(ref target) = app.switching_session {
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
    let chrome_interactive = app.surfaces.active_overlay().is_none()
        && app
            .active_sheet()
            .is_none_or(|kind| kind == crate::sheet::SheetKind::Permission);

    // Project the viewed session's outbox into the small view the
    // persistent queue bar renders. Dispatch order (front pops
    // first) is preserved, so the bar previews the genuine next
    // item to ship. The items are owned snapshots so the bar/modal
    // do not borrow `app` (which is mutated again right after the
    // draw closure).
    //
    // Every outbox item is a next-round item now: a live busy-Enter steer is
    // transcript-owned and never passes through
    // the outbox (ADR-0126), so there is no `steering` slice to
    // exclude from the modal either.
    let queue_items: Vec<render::QueueItemProps> = app
        .pending_dispatch
        .iter()
        .filter(|item| item.session_id == viewed_session_id)
        .map(|item| render::QueueItemProps {
            queued_at_ms: item.queued_at_ms,
            text: item.text.clone(),
        })
        .collect();

    let transcript_render = ui.paint(f, UiKey::Root, |f| {
        render::draw_transcript(
            f,
            &mut layout_map,
            render::TranscriptProps {
                messages: view_messages,
                scroll: app.scroll,
                selection: &app.selection,
                cell_selection: app.drag.cell_info.as_ref(),
                activity: &status,
                backoff_clause: backoff_clause.as_deref(),
                // A pending permission request forces the activity bar on (and
                // tints it warning) so it stays the visible anchor above the
                // permission sheet even if the loop has gone idle.
                awaiting_permission: app.pending_permission.is_some(),
                // ~100ms per phase keeps one breathing cycle near 1.2s
                // (SPINNER_PHASES steps); `breathing_color` wraps modulo.
                spinner_phase: (app.spinner_epoch.elapsed().as_millis() / 100) as usize,
                input: &masked_input,
                byte_cursor: masked_byte_cursor,
                chrome_hidden,
                queue_bar: render::QueueBarProps {
                    items: &queue_items,
                    // "Paused" = items waiting on the server's round boundary
                    // (ADR-0197 M4: the server decides when they ship).
                    paused: app.pending_dispatch.iter().any(|item| {
                        item.session_id == viewed_session_id
                            && item.state == crate::app::QueuedDispatchState::Waiting
                    }),
                    blocked: app.pending_count(viewed_session_id) > 0
                        && app.is_queue_blocked(viewed_session_id),
                    // The legend's keycap comes from the registry, never a
                    // literal: the bar can only advertise a chord that fires
                    // (ADR-0238).
                    expand_key: app
                        .key_overrides
                        .effective_binding(crate::keymap::CommandId::OpenQueue),
                },
                tasks_bar: render::TasksBarProps {
                    tasks: &app.background_tasks,
                },
                persistence_health: app.persistence_health.as_ref(),
                subagent_bar,
                side_banner,
                // ADR-0205: a head legend advertises scene chords, which are
                // suspended the moment an overlay takes the keyboard
                // foreground — so the row is withheld rather than left
                // dimmed-but-readable behind the backdrop.
                page_hints: scene_chrome_hints.then_some(page_hints),
                session_head: Some(render::SessionHead {
                    session_id: viewed_session_id,
                    workspace: &app.current_workspace,
                    role: app.current_role.as_deref(),
                    switching_target: app.switching_session.as_deref(),
                }),
                // View-scoped: the elapsed-timer origin belongs to the viewed
                // session's round (an aside view times the aside's round, not
                // the primary's).
                round_started_at: viewed_chrome.round_started_at,
                hovered_step: chrome_interactive.then_some(app.hovered_step).flatten(),
                focused_target: chrome_interactive.then_some(app.focused_target).flatten(),
                logo: app.logo.as_deref(),
                guidance,
                carousel_index: crate::empty_state::carousel_page_for(
                    app.carousel_epoch.elapsed().as_millis(),
                ),
                theme: &app.theme,
                layout: app.transcript_layout,
                height_cache: Some(&mut height_cache),
            },
        )
    });
    let input_rect = transcript_render.input_rect;
    let hint_rect = transcript_render.hint_rect;
    let content_lines = transcript_render.content_lines;
    let view_height = transcript_render.view_height;
    let sticky = transcript_render.sticky;
    for (key, rect) in &transcript_render.footer.rows {
        let key = match key {
            render::FooterRowId::TopGap => continue,
            render::FooterRowId::PersistenceHealth => continue,
            render::FooterRowId::Queue => UiKey::Queue,
            render::FooterRowId::Tasks => continue,
            render::FooterRowId::Activity => UiKey::Activity,
            render::FooterRowId::Composer => UiKey::Composer,
            render::FooterRowId::ModelBar => UiKey::ModelBar,
        };
        ui.mount(key, *rect);
    }
    if let Some(rect) = layout_map.transcript_content_rect() {
        ui.mount(UiKey::Transcript, rect);
    }

    // The input-action hint bar (with model/context metadata on
    // the right) lives directly below the input box. It is drawn
    // before the composer because it borrows `view_messages` (an
    // immutable borrow of `app`) while `draw_composer` needs a
    // mutable borrow of `app.input_scroll`.
    // The permission sheet takes over the hint line as well as the
    // input box, so suppress the hint bar while it is open.
    if !chrome_hidden
        && hint_rect.height > 0
        && app.active_sheet() != Some(crate::sheet::SheetKind::Permission)
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
            .provider_picker
            .rows
            .iter()
            .find(|row| row.id == app.current_provider);
        // The `@<instance>` suffix after the model name — the
        // instance's display name, so identical models served by
        // different instances stay attributable.
        let hint_instance = active_provider_row.map(|row| row.name.as_str());
        let hint_reasoning = effective_reasoning_effort(app);
        let model_available = active_provider_row
            .is_none_or(|row| row.models.iter().any(|m| m == &app.current_model));
        let busy = app.running_sessions.contains(viewed_session_id);
        let _ = busy; // keys-row input; consumed by the composer draw below
        // `/retry` affordance (ADR-0128): offered exactly while the viewed
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
                    current_model: &app.current_model,
                    model_available,
                    provider_name: hint_instance,
                    reasoning_effort: hint_reasoning,
                    context_tokens: app.context_tokens.map(|snapshot| snapshot.tokens),
                    context_window: app.active_model_context_window(),
                },
                &app.theme,
                &app.key_overrides,
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

    // The input box is only shown when no overlay modal is open. The
    // `focused` flag drops the panel to its dim "blurred" palette and
    // hides the caret whenever keyboard focus is on the conversation
    // stream (Browse zone), so the user can see at a glance which
    // surface the next keypress will land on. A pending permission
    // request replaces the composer with the inline permission sheet.
    if !chrome_hidden {
        if app.active_sheet() == Some(crate::sheet::SheetKind::Permission) {
            if let Some(request) = app.pending_permission.as_ref() {
                // Extend the slot down by the composer/hint gap plus
                // the hint-line height so the sheet also covers
                // (replaces) the bar below the input.
                let permission_rect = nuotc::Rect::new(
                    input_rect.x,
                    input_rect.y,
                    input_rect.width,
                    input_rect.height + crate::design::COMPOSER_HINT_GAP_ROWS + hint_rect.height,
                );
                let max_scroll = render::draw_permission_sheet(
                    f,
                    ui,
                    request,
                    app.modal_index,
                    app.permission_confirm_always,
                    app.permission_show_details,
                    app.permission_scroll,
                    app.pending_permission_depth,
                    permission_rect,
                    &app.theme,
                    &app.selection,
                    &mut layout_map,
                );
                app.permission_max_scroll = max_scroll;
                app.permission_scroll = app.permission_scroll.min(app.permission_max_scroll);
            }
        } else if matches!(
            app.active_dialog(),
            Some(DialogKind::Connections | DialogKind::Models)
        ) || app.surfaces.contains_sheet(SheetKind::ModelEditor)
            || app.surfaces.contains_sheet(SheetKind::CustomProvider)
        {
            // These modals borrow the input line as their own field
            // (filter / key+model / history-query), so the composer
            // underneath would only duplicate the same `app.input` the
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
            // step carries keyboard focus (Ctrl+↑/↓), the composer drops
            // to its dim "blurred" palette and hides the caret so the
            // user can see at a glance that the next keypress targets
            // the step, not the input box. Typing into the box clears
            // the focus and re-brightens it immediately.
            //
            // `show_caret` comes straight from the single source of
            // truth (`App::caret_visible`): in this branch the composer
            // is the only possible caret surface (the caret-owning
            // modals are handled by the `skip` branch above, and subagent
            // zoom is excluded by the `!in_subagent_view` gate), so
            // `caret_visible` reduces to "no step focus, no selection"
            // — exactly the old hand-rolled condition, without the risk
            // of drifting from the hide/show state machine.
            // ADR-0174: browse focus joins step selection as the second
            // "keys act on the transcript" signal — a click anywhere in
            // the transcript content parks attention there and dims the
            // composer panel until a composer click or a keystroke
            // hands it back.
            let step_focused = app.focused_target.is_some() || app.transcript_focused;
            let show_caret = scene_owns_caret && !step_focused;
            let composer_focused = !step_focused && (!has_overlay || scene_owns_caret);
            // A fully-typed known `/command` is painted in bold +
            // accent color so it reads as a resolved command
            // rather than prose; an unmatched `/`-prefix keeps
            // the normal text color.
            let slash_len = resolved_slash_command_len(&app.input, &app.command_catalog);
            let byte_cursor = app.byte_cursor();
            let image_count = app.pending_images.len();
            let paste_count = app.pending_text_pastes.len();
            // Compose-target derivation happens while `&app` borrows are
            // still immutable; the result is an owned value the composer can
            // consume after taking its mutable borrows.
            let busy = app.running_sessions.contains(viewed_session_id);
            let active_extension = app.active_composer_extension();
            let composer_hints = {
                use crate::components::composer_hints::{
                    ComposerHints, compose_target_for_extension,
                };
                ComposerHints {
                    compose_target: compose_target_for_extension(
                        busy,
                        Some(app.composer_send_mode),
                        slash_len.is_some() || app.input.starts_with('/'),
                        active_extension,
                        app.history_index.is_some(),
                    ),
                    can_retry: !busy && viewed_chrome.can_retry,
                    history_recall: app.history_recall_badge(),
                    recall_draft_saved: !app.history_draft.is_empty()
                        || !app.history_draft_images.is_empty()
                        || !app.history_draft_text_pastes.is_empty(),
                    toggle_mode_key: app
                        .surface_overrides
                        .effective_binding(crate::keymap::SurfaceVerb::ToggleSendMode),
                }
            };
            let composer_options = render::ComposerDrawOptions {
                focused: composer_focused,
                show_caret,
                follow_caret: app.input_scroll_follow_cursor,
                record: true,
                image_count,
                paste_count,
                hints: composer_hints,
            };
            match slash_len {
                Some(len) => render::draw_composer_highlighted(
                    ComposerProps {
                        frame: f,
                        input_rect,
                        theme: &app.theme,
                        layout_map: &mut layout_map,
                        input_scroll: &mut app.input_scroll,
                        selection: &app.selection,
                    },
                    ComposerText {
                        input: &app.input,
                        byte_cursor,
                    },
                    composer_options,
                    len,
                ),
                None if app.input_scroll_follow_cursor => render::draw_composer(
                        ComposerProps {
                            frame: f,
                            input_rect,
                            theme: &app.theme,
                            layout_map: &mut layout_map,
                            input_scroll: &mut app.input_scroll,
                            selection: &app.selection,
                        },
                        ComposerText {
                            input: &app.input,
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
                            theme: &app.theme,
                            layout_map: &mut layout_map,
                            input_scroll: &mut app.input_scroll,
                            selection: &app.selection,
                        },
                        ComposerText {
                            input: &app.input,
                            byte_cursor,
                        },
                        composer_options,
                    ),
            }
        }
    }

    // Now that `view_messages` is no longer borrowed, persist the
    // per-frame layout state back onto `app` for the next iteration
    // and for click routing.
    // Restore the height cache (populated/refreshed during this draw)
    // so the next frame can reuse it.
    app.layout_height_cache = height_cache;
    app.content_lines = content_lines;
    app.view_height = view_height;
    app.max_scroll = content_lines
        .saturating_sub(view_height as usize)
        .min(u16::MAX as usize) as u16;
    // Hit-test rects for the footer bars, resolved from the one registry the
    // renderer placed this frame (`TranscriptRender::footer`) — one source
    // of truth instead of per-bar plumbing.
    // The composer panel's own rect, for the spatial mouse router (wheel
    // ticks and selection edge-autoscroll inside the box drive the input's
    // viewport, not the transcript). Zero-height / absent rows resolve to
    // `None` — a collapsed or hidden composer owns no pointer cell.
    match sticky {
        Some(info) => {
            app.sticky_step = Some(info.message_idx);
            ui.mount(UiKey::Sticky, info.rect);
            app.sticky_summary_line = Some(info.summary_line);
        }
        None => {
            app.sticky_step = None;
            app.sticky_summary_line = None;
        }
    }

    // Interaction sheets (ADR-0173 §3): the AI-initiated sheets occupy the
    // composer slot — the same bottom edge, extended over the hint bar —
    // while the transcript behind them stays live. The question and
    // input-injection sheets paint their own panel over the composer's
    // slot; the permission sheet replaced the slot entirely above.
    if !chrome_hidden {
        match app.active_sheet() {
            Some(crate::sheet::SheetKind::Question) => {
                if let Some(ref qmodel) = app.question {
                    let question_rect = nuotc::Rect::new(
                        input_rect.x,
                        input_rect.y,
                        input_rect.width,
                        input_rect.height
                            + crate::design::COMPOSER_HINT_GAP_ROWS
                            + hint_rect.height,
                    );
                    render::draw_question_modal(
                        f,
                        ui,
                        qmodel.request(),
                        qmodel.current(),
                        qmodel.selected(),
                        qmodel.other_text(),
                        qmodel.highlight(),
                        &mut app.question_scroll,
                        app.question_modal_follow,
                        app.pending_question_depth,
                        question_rect,
                        overlay_owns_caret,
                        &app.theme,
                    );
                }
            }
            Some(crate::sheet::SheetKind::InputInjection) => {
                if let Some(ref req) = app.pending_input {
                    ui.mount(
                        UiKey::Sheet(crate::sheet::SheetKind::InputInjection),
                        input_rect,
                    );
                    render::draw_input_injection(
                        f,
                        req,
                        &app.input,
                        app.cursor_position,
                        input_rect,
                        &app.theme,
                    );
                }
            }
            _ => {}
        }
    }

    // Completion menu: slash commands or `@path` file mentions.
    // Honors `completion_dismissed` so Esc / Enter-commit keep the
    // popup hidden until the next edit clears the latch. Also
    // suppressed for a fully-typed command whose exact match is the
    // text already in the box — that is a *resolved* state (the
    // composer paints it bold + accent), the popup has nothing left
    // to offer, and ↑/↓ keep walking history instead of cycling a
    // single pinned row.
    if app.surfaces.active_overlay().is_none()
        && app.active_sheet().is_none()
        && !app.completion_dismissed
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
            c.replace_start == 0 && c.replace_end == app.input.len() && c.label == app.input
        });
        if !completions.is_empty() && !exact_match {
            // Anchor the popup relative to the active trigger token
            // (e.g. the `@` in a mention or `/` command) in both X and Y,
            // tracking the cursor's wrapped row and scroll offset so the menu
            // hovers immediately above the line being typed.
            let anchor = completion_anchor(
                &app.input,
                app.byte_cursor(),
                input_rect,
                app.input_scroll,
                app.completion_kind(),
            );
            render::draw_completion_menu(
                f,
                &mut layout_map,
                Some(&mut ui),
                &completions,
                app.suggestion_index,
                anchor,
                &app.theme,
            );
        }
    }

    // Recess the live surface for the open modal: darken it in place
    // (Dim), occlude it fully (Takeover), or leave it untouched (None).
    // Done after the transcript + chrome are drawn and before the modal
    // panel so the panel overpaints its own crisp area on top of the
    // recessed background.
    ui.mount(UiKey::Backdrop, f.area());
    ui.paint(f, UiKey::Backdrop, |f| {
        render::recess_backdrop(f, recess, &app.theme)
    });

    let spinner_phase = (app.spinner_epoch.elapsed().as_millis() / 100) as usize;

    // The dashboard reports its true list-body height through this
    // slot (its body is not the centered panel-minus-chrome the
    // shared post-match math assumes). Reset each frame; only the
    // `SceneKind::Dashboard` arm sets it.
    let mut dashboard_list_body_height: Option<u16> = None;

    // Overlays and Scenes (ADR-0205)
    let drawn_modal_rect = if let Some(overlay) = app.surfaces.active_overlay() {
        match overlay {
            OverlaySurface::Dialog(d) if app.dialog_keys() => {
                let app_ctx = crate::keymap::AppContext {
                    has_overlay: true,
                    active_dialog: Some(d),
                    is_responding: viewed_running,
                    has_selection: !matches!(
                        app.selection,
                        crate::model::selection::SelectionState::None
                    ),
                    has_running_task: viewed_running,
                    queue_count: app.pending_dispatch.len(),
                    has_session: app.has_session(),
                    scene: app.current_scene(),
                };
                Some(render::draw_dialog_keys(
                    f,
                    d,
                    app.surfaces.dialogs.keys_scroll_mut(d),
                    &app_ctx,
                    &app.theme,
                    &app.selection,
                    &mut layout_map,
                ))
            }
            OverlaySurface::Dialog(_) => {
                // Route through the encapsulated entity's `DialogView::render`
                // (ADR-0035 §1). The entity is taken out of its stack entry so
                // it can render against a disjoint `&App` borrow, then put back.
                if let Some(mut ent) = app.surfaces.take_active_view() {
                    let startup_picker =
                        app.startup_overlay == crate::StartupOverlay::SessionsPicker;
                    let activity_height = render::footer_rect(
                        &transcript_render.footer,
                        render::FooterRowId::Activity,
                    )
                    .map_or(0, |r| r.height);
                    let rect = {
                        let mut ctx = crate::surfaces::DialogRenderCtx {
                            app,
                            layout_map: &mut layout_map,
                            selection: &app.selection,
                            theme: &app.theme,
                            spinner_phase,
                            viewed_session_id,
                            startup_picker,
                            input_rect: Some(input_rect),
                            activity_height,
                            overlay_owns_caret,
                        };
                        ent.render(f, f.area(), &mut ctx)
                    };
                    app.surfaces.put_active_view(ent);
                    rect
                } else {
                    None
                }
            },
            OverlaySurface::Sheet(s) => match s {
                SheetKind::ModelEditor => {
                    if let Some(target) = app.editor_target.as_deref()
                        && (target.starts_with("web_credential:")
                            || target.starts_with("web_endpoint:"))
                    {
                        let endpoint = target.starts_with("web_endpoint:");
                        Some(render::draw_web_value_editor(
                            f,
                            &format!("Configure {}", app.editor_model),
                            if endpoint { "Endpoint" } else { "API token" },
                            &app.input,
                            app.cursor_position,
                            !endpoint,
                            overlay_owns_caret,
                            &app.theme,
                        ))
                    } else {
                        let title = if app.editor_model_settings_only {
                            app.editor_model.clone()
                        } else {
                            app.editor_target
                                .as_deref()
                                .and_then(|id| app.provider_picker.rows.iter().find(|r| r.id == id))
                                .map(|r| r.name.clone())
                                .unwrap_or_else(|| "model".to_string())
                        };
                        let effort = app
                            .editor_model_settings_only
                            .then_some(app.editor_effort.as_str());
                        // The ladder captured from the snapshot when the editor
                        // opened. Deliberately NOT re-derived with
                        // `resolve_model`: this binary does not link
                        // `nuo-providers`, so the provider baseline tables are
                        // absent and a client-side resolve returns an empty
                        // ladder — which collapsed the node slider to the
                        // value-only row. Empty stays empty (a route with no
                        // effort knob keeps the value-only fallback).
                        let effort_levels: Vec<String> = if app.editor_model_settings_only {
                            app.editor_effort_levels.clone()
                        } else {
                            Vec::new()
                        };
                        let thinking = app
                            .editor_model_settings_only
                            .then_some(app.editor_thinking)
                            .filter(|_| app.editor_thinking_available);
                        let overrides = app
                            .editor_model_settings_only
                            .then_some((app.editor_vision_override, app.editor_tool_override));
                        Some(render::draw_model_editor(
                            f,
                            &title,
                            &app.input,
                            app.cursor_position,
                            !app.editor_model_settings_only,
                            app.editor_field,
                            overlay_owns_caret,
                            effort,
                            &effort_levels,
                            thinking,
                            overrides,
                            &app.theme,
                        ))
                    }
                }
                SheetKind::ProviderPreset => Some(render::draw_preset_chooser(
                    app.preset_choice,
                    f,
                    &app.theme,
                    &mut app.preset_scroll,
                )),
                SheetKind::OAuthPending => {
                    let title: &'static str = match &app.custom_auth {
                        nuo_wire::ConnectionAuth::Subscription { provider } => {
                            match provider.as_ref() {
                                "chatgpt" => "ChatGPT Subscription",
                                "copilot" => "Copilot",
                                "xai" => "xAI",
                                "google-antigravity" => "Google Antigravity",
                                "qoder" => "Qoder",
                                "opencode" | "opencode-go" => "OpenCode Go",
                                _ => "Subscription",
                            }
                        }
                        nuo_wire::ConnectionAuth::ApiKey => "OAuth",
                    };
                    Some(render::draw_oauth_pending(
                        title,
                        &app.oauth_pending_message,
                        &app.oauth_pending_url,
                        &app.oauth_pending_user_code,
                        app.oauth_pending_error.as_deref(),
                        app.oauth_selected_item,
                        f,
                        &app.theme,
                        &mut app.oauth_scroll,
                        Some(&mut ui),
                        &app.selection,
                        &mut layout_map,
                    ))
                }
                SheetKind::CustomProvider => {
                    let editing = app.custom_is_editing();
                    let title = if editing {
                        format!("Edit — {}", app.custom_name)
                    } else {
                        crate::provider_label_for(app.custom_provider_id.as_deref())
                    };
                    let protocol_display = app
                        .custom_protocol_wire
                        .parse::<nuo_wire::WireProtocol>()
                        .map(|p| p.display_name())
                        .unwrap_or(&app.custom_protocol_wire);
                    Some(render::draw_custom_provider_editor(
                        render::CustomEditorProps {
                            fields: &app.custom_fields,
                            field: app.custom_field,
                            editing,
                            custom: app.custom_provider_id.as_deref()
                                == Some(crate::providers::CUSTOM_TEMPLATE.id),
                            title: &title,
                            name_buf: &app.custom_name,
                            base_url_buf: &app.custom_base_url,
                            token_buf: &app.custom_token,
                            model_buf: &app.custom_model,
                            protocol_display,
                            identity_display: app.custom_client_identity.label(),
                            url_hint: &app.custom_url_hint,
                            input: &app.input,
                            cursor_position: app.cursor_position,
                        },
                        f,
                        &app.theme,
                        &mut app.custom_scroll,
                        overlay_owns_caret,
                    ))
                }
                _ => None,
            },
        }
    } else {
        match app.current_scene() {
            SceneKind::Dashboard => {
                let rects = render::draw_dashboard(
                    f,
                    crate::overlays::dashboard::DashboardProps {
                        rows: &app.host_sessions,
                        selected: app
                            .modal_index
                            .min(app.host_sessions.len().saturating_sub(1)),
                        focus: app.host_focus,
                        list_scroll: &mut app.host_scroll,
                        list_follow: app.host_modal_follow,
                        detail_scroll: &mut app.host_detail_scroll,
                        log: &app.host_console_log,
                        prompting: app.host_prompting,
                        prompt_create_new: app.host_prompt_new,
                        prompt_text: &app.input,
                        current_session_id: viewed_session_id,
                        show_caret: scene_prompt_owns_caret,
                        session_head: Some(render::SessionHead {
                            session_id: viewed_session_id,
                            workspace: &app.current_workspace,
                            role: app.current_role.as_deref(),
                            switching_target: app.switching_session.as_deref(),
                        }),
                        unattended: app.unattended,
                        confined: app.confined,
                    },
                    &app.theme,
                );
                dashboard_list_body_height = Some(rects.list_body.height);
                if let Some(preview_id) = &app.host_preview {
                    let row = app.host_sessions.iter().find(|r| &r.id == preview_id);
                    render::draw_session_preview(f, row, &mut app.host_preview_scroll, &app.theme);
                }
                Some(rects.area)
            }
            SceneKind::Settings => {
                let breadcrumbs_str = if app.in_side_view {
                    "Main › Aside › Settings"
                } else if app.in_subagent_view() {
                    "Main › Subagent › Settings"
                } else {
                    "Main › Settings"
                };
                let rects = render::draw_settings_view(
                    f,
                    render::SettingsProps {
                        category_index: app.config_category,
                        detail_index: app.config_detail_index,
                        hover_index: app.config_hover_index,
                        focus: app.config_focus,
                        color_scheme: &app.color_scheme,
                        custom_color_scheme: &app.custom_color_scheme,
                        websearch: app.websearch_config.as_ref(),
                        workspace: &app.current_workspace,
                        category_scroll: &mut app.config_scroll,
                        detail_scroll: &mut app.config_detail_scroll,
                        breadcrumbs: Some(breadcrumbs_str),
                        theme: &app.theme,
                        profile: &app.profile,
                        tui_config: &app.tui_config,
                        session_head: Some(render::SessionHead {
                            session_id: viewed_session_id,
                            workspace: &app.current_workspace,
                            role: app.current_role.as_deref(),
                            switching_target: app.switching_session.as_deref(),
                        }),
                        unattended: app.unattended,
                        confined: app.confined,
                    },
                );
                app.config_selected_rect = rects.selected_row_rect;
                if let Some(row_rect) = rects.selected_row_rect {
                    ui.mount(UiKey::SettingsOption(app.config_detail_index), row_rect);
                }
                // Full-block pointer targets for every visible row, so the row
                // under the mouse (label *or* description line) lights up. Mounted
                // after the cursor's own target so the taller block wins the hit
                // test; both resolve to the same row index, so the choice is moot.
                for (index, row_rect) in rects.row_rects {
                    ui.mount(UiKey::SettingsRow(index), row_rect);
                }
                if let Some((ref mut state, ref mut anchor)) = app.config_dropdown {
                    if let Some(target_rect) = app.config_selected_rect
                        && anchor.placement
                            != crate::components::dropdown::DropdownPlacement::CenterScreen
                    {
                        anchor.target_rect = target_rect;
                    }
                    let popup_area = crate::components::dropdown::draw_dropdown(
                        f,
                        state,
                        anchor,
                        &app.theme,
                        f.area(),
                    );
                    ui.mount(UiKey::ConfigDropdown, popup_area);
                }
                Some(rects.area)
            }
            SceneKind::Conversation | SceneKind::TaskInspection | SceneKind::Aside => None,
        }
    };

    // Provider-delete confirm overlay: a sub-layer painted *on top
    // of* the Connections list. Drawn after the picker so it
    // overpaints its own dimmed backdrop + centered panel, leaving
    // the list visible (dimmed) behind it. Only present while a
    // deletion is staged from `Shift+D`.
    if app.active_dialog() == Some(DialogKind::Connections)
        && let Some(ref pending_id) = app.pending_provider_delete
    {
        let provider_name = app
            .provider_picker
            .rows
            .iter()
            .find(|r| &r.id == pending_id)
            .map(|r| r.name.clone())
            .unwrap_or_else(|| pending_id.clone());
        let rect = render::draw_provider_delete_confirm(
            f,
            &provider_name,
            match app.provider_delete_focus {
                ProviderDeleteChoice::Cancel => ConfirmChoice::Cancel,
                ProviderDeleteChoice::Delete => ConfirmChoice::Delete,
            },
            &app.theme,
        );
        ui.mount(UiKey::ProviderDelete, rect);
    }

    // Urgent confirmation toasts (Esc interrupt confirmation or Ctrl+C quit confirmation)
    // take precedence over informational toasts (copy, command acknowledgment)
    // so active safety prompts are never obscured.
    if app.esc_armed() {
        render::draw_armed_toast(f, "Esc again interrupts", &app.theme);
    } else if app.ctrl_c_armed() {
        render::draw_armed_toast(f, "press Ctrl-c again to exit", &app.theme);
    } else if app.copy_toast_until.is_some() {
        render::draw_copy_toast(
            f,
            &app.copy_toast_message,
            app.copy_toast_failed,
            &app.theme,
        );
    } else if app.notice_toast_until.is_some() {
        // A toast-surfaced command acknowledgment (e.g.
        // `/delegate on`). Rendered only when no higher-priority toast is
        // showing, since they share the same top-right slot.
        render::draw_notice_toast(
            f,
            &app.notice_toast_message,
            app.notice_toast_severity,
            &app.theme,
        );
    }

    // Floating which-key card while the `Ctrl+X` scene namespace is armed. The
    // leave row spells the *resolved* action for the current surface stack (an
    // overlay dismiss, a scene exit, or a spent gesture at the home scene)
    // rather than a fixed promise (ADR-0238).
    let close_label = crate::components::which_key::close_label_for(
        app.active_dialog().is_some(),
        app.current_scene() != crate::surfaces::SceneKind::Conversation,
    );
    crate::components::which_key::draw_which_key_overlay(
        f,
        &app.theme,
        app.scene_namespace_armed,
        close_label,
        f.area(),
    );

    ui.stage_document(layout_map);

    // Capture the open modal's body height for page-scroll step
    // sizing. The renderer returns the full panel rect; the body is
    // that rect minus the header/footer/padding chrome. All
    // centered modals that paint a scrollable body use the same
    // `modal_frame(header, footer)` chrome, so the row count is the
    // shared `modal_chrome_rows` for a header+footer spec. Stays 0
    // for modals that return no rect (Permission sheet, which
    // scrolls the transcript behind it via `view_height` instead),
    // so the page step falls back to the transcript height there.
    app.modal_body_height = match dashboard_list_body_height {
        // The dashboard's scroll body is its list pane, whose height
        // was reported directly by the renderer.
        Some(h) => h,
        None => drawn_modal_rect
            .map(|r| {
                r.height
                    .saturating_sub(crate::primitives::modal_chrome_rows(
                        crate::primitives::ModalSpec {
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
    if let (Some(rect), Some(overlay)) = (drawn_modal_rect, app.surfaces.active_overlay()) {
        ui.mount(UiKey::Overlay(overlay), rect);
    }
}

/// ADR-0240 [INV-MCP-04]: Extract in-flight MCP connection status during system bootstrapping.
fn mcp_connecting_status(app: &App) -> Option<String> {
    let snapshot = app.session_context.as_ref()?;
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

/// The conversation scene's row-2 context (ADR-0024): the chat's title. The
/// title is derived from the first real chat prompt the user drove the
/// conversation with — a slash command or steering insert is not a title — and
/// cleaned to a single bounded line by the same rule the session titler uses
/// ([`nuo_wire::clean_title`]). `None` before the first real prompt, so the
/// scene row shows only its label.
fn conversation_title(messages: &[TranscriptMessage]) -> Option<String> {
    let prompt = messages.iter().find(|m| {
        m.role == nuo_wire::Role::User && m.origin == crate::model::document::UserMessageOrigin::Chat
    })?;
    // `clean_title` collapses to the first non-empty line and caps the length
    // (with an ellipsis), so a multi-line first prompt still yields a tidy
    // one-line title.
    nuo_wire::clean_title(&prompt.raw)
}
