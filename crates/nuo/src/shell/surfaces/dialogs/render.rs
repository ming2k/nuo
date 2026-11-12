//! Dialog rendering: one match arm per entity kind.

use super::entities::*;
use super::*;

/// Render one dialog entity from its own state plus the injected immutable
/// context (`DialogView::render`). The entity's fields are the only mutable
/// dialog state; `ctx` carries read-only server model data and layout context.
#[allow(clippy::expect_used)]
pub(super) fn render_dialog(
    view: &mut dyn DialogView,
    frame: &mut Frame,
    area: Rect,
    ctx: &mut DialogRenderCtx<'_>,
) -> Option<Rect> {
    let _ = area;
    let app = ctx.app;
    match view.kind() {
        DialogKind::Tools => {
            let d = view
                .as_any_mut()
                .downcast_mut::<ToolsDialog>()
                .expect("kind");
            Some(crate::shell::overlays::draw_tools_modal(
                frame,
                app.session_context.as_ref(),
                d.index,
                &mut d.scroll,
                d.follow,
                d.info_detail,
                &mut d.info_scroll,
                ctx.theme,
            ))
        }
        DialogKind::Mcp => {
            let d = view.as_any_mut().downcast_mut::<McpDialog>().expect("kind");
            Some(crate::shell::overlays::draw_mcp_modal(
                frame,
                app.session_context.as_ref(),
                d.index,
                &mut d.scroll,
                d.follow,
                ctx.theme,
            ))
        }
        DialogKind::Skills => {
            let d = view
                .as_any_mut()
                .downcast_mut::<SkillsDialog>()
                .expect("kind");
            Some(crate::shell::overlays::draw_skills_modal(
                frame,
                app.session_context.as_ref(),
                d.index,
                d.expanded,
                &mut d.scroll,
                ctx.theme,
            ))
        }
        DialogKind::Permissions => {
            let d = view
                .as_any_mut()
                .downcast_mut::<PermissionsDialog>()
                .expect("kind");
            Some(crate::shell::overlays::draw_permissions_manager(
                frame,
                app.session_context.as_ref(),
                d.index,
                &mut d.scroll,
                ctx.theme,
            ))
        }
        DialogKind::UsageStats => {
            let d = view
                .as_any_mut()
                .downcast_mut::<UsageStatsDialog>()
                .expect("kind");
            let loading = app.usage_stats.is_none();
            let report = app.usage_stats.clone().unwrap_or_default();
            Some(crate::shell::overlays::draw_usage_stats_modal(
                frame,
                &report,
                loading,
                &mut d.scroll,
                ctx.theme,
                ctx.selection,
                ctx.layout_map,
            ))
        }
        DialogKind::Quotas => {
            let d = view
                .as_any_mut()
                .downcast_mut::<QuotasDialog>()
                .expect("kind");
            let loading = app.provider_quotas.is_none();
            Some(crate::shell::overlays::draw_quotas_modal(
                frame,
                app.provider_quotas.as_ref(),
                loading,
                d.index,
                &mut d.scroll,
                d.expanded,
                ctx.theme,
                ctx.selection,
                ctx.layout_map,
            ))
        }
        DialogKind::ThreadStats => {
            let d = view
                .as_any_mut()
                .downcast_mut::<ThreadStatsDialog>()
                .expect("kind");
            let report = app.token_source_report(ctx.viewed_session_id);
            let loading = app.token_ledger.is_none() && report.is_none();
            let report = report.unwrap_or_default();
            Some(crate::shell::overlays::draw_thread_stats_modal(
                frame,
                &report,
                crate::shell::render::ContextUsageProps {
                    snapshot: app.context_tokens,
                    window_tokens: Some(app.active_model_context_window()),
                    draft_content_tokens: nuo_protocol::count_tokens(&app.input),
                    draft_tokens: nuo_protocol::estimate_draft_tokens(&app.input),
                },
                loading,
                &mut d.scroll,
                ctx.theme,
                ctx.selection,
                ctx.layout_map,
            ))
        }
        DialogKind::SessionTrace => {
            let d = view
                .as_any_mut()
                .downcast_mut::<SessionTraceDialog>()
                .expect("kind");
            let report = app.token_source_report(ctx.viewed_session_id);
            let loading = app.token_ledger.is_none() && report.is_none();
            let report = report.unwrap_or_default();
            Some(crate::shell::overlays::draw_session_trace_modal(
                frame,
                &report,
                crate::shell::render::ContextUsageProps {
                    snapshot: app.context_tokens,
                    window_tokens: Some(app.active_model_context_window()),
                    draft_content_tokens: nuo_protocol::count_tokens(&app.input),
                    draft_tokens: nuo_protocol::estimate_draft_tokens(&app.input),
                },
                d.index
                    .min(crate::shell::render::telemetry_round_count(&report).saturating_sub(1)),
                d.detail,
                d.turn,
                d.turn_cursor,
                app.last_submit_ms,
                loading,
                &mut d.scroll,
                ctx.theme,
                ctx.selection,
                ctx.layout_map,
            ))
        }
        DialogKind::Queue => {
            let d = view
                .as_any_mut()
                .downcast_mut::<QueueDialog>()
                .expect("kind");
            // Project through the one outbox definition, so the listed rows and
            // the rows `D`/`Enter` address are the same set by construction.
            let items: Vec<crate::shell::render::QueueItemProps> = app
                .outbox_projection(ctx.viewed_session_id)
                .into_iter()
                .map(|item| crate::shell::render::QueueItemProps {
                    queued_at_ms: item.queued_at_ms,
                    text: item.text.clone(),
                    is_steer: item.state == crate::shell::app::QueuedDispatchState::Steering,
                })
                .collect();
            let blocked = app.pending_count(ctx.viewed_session_id) > 0
                && app.is_queue_blocked(ctx.viewed_session_id);
            Some(crate::shell::overlays::draw_queue_modal(
                frame,
                crate::shell::render::QueueModalProps {
                    items: &items,
                    blocked,
                },
                d.index,
                &mut d.scroll,
                d.follow,
                ctx.theme,
            ))
        }
        DialogKind::Threads => {
            let d = view
                .as_any_mut()
                .downcast_mut::<ThreadsDialog>()
                .expect("kind");
            let projected_count = crate::shell::overlays::session::project_session_rows(
                &app.sessions_overview,
                Some(&d.expanded),
            )
            .len();
            Some(crate::shell::overlays::draw_sessions_modal(
                frame,
                crate::shell::overlays::session::SessionsModalProps {
                    sessions: &app.sessions_overview,
                    expanded_sessions: Some(&d.expanded),
                    selected: d.index.min(projected_count.saturating_sub(1)),
                    scroll: &mut d.scroll,
                    follow: d.follow,
                    startup_picker: ctx.startup_picker,
                    spinner_phase: ctx.spinner_phase,
                    session_info_detail: d.info_detail,
                    session_detail: app.session_detail.as_ref(),
                    session_info_scroll: &mut d.info_scroll,
                    sessions_loading: d.loading,
                    running_sessions: Some(&app.running_sessions),
                },
                ctx.theme,
                ctx.selection,
                ctx.layout_map,
            ))
        }
        DialogKind::SessionTree => {
            let d = view
                .as_any_mut()
                .downcast_mut::<SessionTreeDialog>()
                .expect("kind");
            Some(crate::shell::overlays::draw_tree_modal(
                frame,
                &app.session_tree,
                d.index,
                &mut d.scroll,
                d.follow,
                ctx.theme,
            ))
        }
        DialogKind::Connections => {
            let d = view
                .as_any_mut()
                .downcast_mut::<ConnectionsDialog>()
                .expect("kind");
            let providers = app.providers_filtered();
            Some(crate::shell::overlays::draw_connections_modal(
                frame,
                ctx.layout_map,
                crate::shell::overlays::provider::connections::ConnectionsModalProps {
                    providers: &providers,
                    modal_index: d.index,
                    query: &d.query.text,
                    cursor_position: d.query.cursor,
                    scroll: &mut d.scroll,
                    follow_selection: d.follow,
                    search: d.search,
                    show_caret: ctx.overlay_owns_caret,
                    connection_info_detail: d.info_detail,
                    connection_detail: app.connection_detail.as_ref(),
                    connection_info_scroll: &mut d.info_scroll,
                    spinner_phase: ctx.spinner_phase,
                    connection_info_standalone: d.info_standalone,
                    refreshing: d.refreshing,
                    connection_usages: Some(&app.connection_usages),
                },
                ctx.theme,
                ctx.selection,
            ))
        }
        DialogKind::Models => {
            let d = view
                .as_any_mut()
                .downcast_mut::<ModelsDialog>()
                .expect("kind");
            let models = app.models_flat_filtered();
            if !models.is_empty() {
                if d.index >= models.len() {
                    d.index = models.len().saturating_sub(1);
                }
                if !d.info_detail
                    && let Some(m) = models.get(d.index)
                    && !m.usable
                    && let Some(usable_idx) = models.iter().position(|m| m.usable)
                {
                    d.index = usable_idx;
                }
            }
            Some(crate::shell::overlays::draw_models_modal(
                frame,
                ctx.layout_map,
                crate::shell::overlays::provider::models::ModelsModalProps {
                    models: &models,
                    modal_index: d.index,
                    query: &d.query.text,
                    cursor_position: d.query.cursor,
                    scroll: &mut d.scroll,
                    follow_selection: d.follow,
                    search: d.search,
                    show_caret: ctx.overlay_owns_caret,
                    refreshing: d.refreshing,
                    spinner_phase: ctx.spinner_phase,
                    info_detail: d.info_detail,
                    info_scroll: &mut d.info_scroll,
                    reveal_active: &mut d.reveal_active,
                },
                ctx.theme,
                ctx.selection,
            ))
        }
        DialogKind::HistorySearch => {
            let d = view
                .as_any_mut()
                .downcast_mut::<HistorySearchDialog>()
                .expect("kind");
            let input_rect = ctx.input_rect?;
            let ranked = app.history_rows();
            if !ranked.is_empty() && d.index >= ranked.len() {
                d.index = ranked.len().saturating_sub(1);
            }
            crate::shell::overlays::draw_history_panel(
                frame,
                crate::shell::overlays::history::HistoryPanelProps {
                    history: &app.input_history,
                    ranked: &ranked,
                    modal_index: d.index,
                    scroll: &mut d.scroll,
                    follow_selection: d.follow,
                    input_rect,
                    activity_height: ctx.activity_height,
                },
                ctx.theme,
            )
        }
        DialogKind::Switcher => {
            let d = view
                .as_any_mut()
                .downcast_mut::<SwitcherDialog>()
                .expect("kind");
            let app_ctx = crate::shell::keymap::AppContext {
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
            };
            let entries = crate::shell::overlays::command_palette::filter_palette_commands(
                &d.query.text,
                &app.recent_commands,
                &app_ctx,
            );
            let show_caret =
                app.caret_visible() && app.caret_owner() == crate::shell::CaretOwner::Overlay;
            Some(crate::shell::overlays::draw_command_palette(
                frame,
                crate::shell::overlays::command_palette::CommandPaletteProps {
                    query: &d.query.text,
                    cursor_position: d.query.cursor,
                    entries: &entries,
                    selected_index: d.selected,
                    scroll: &mut d.scroll,
                    show_caret,
                },
                ctx.theme,
                ctx.selection,
                ctx.layout_map,
            ))
        }
        DialogKind::PortShare => {
            let d = view
                .as_any_mut()
                .downcast_mut::<PortShareDialog>()
                .expect("kind");
            Some(crate::shell::overlays::draw_port_modal(
                frame,
                d.port,
                d.active,
                d.token.as_deref(),
                &d.local_path,
                d.notice.as_deref(),
                d.loading,
                ctx.theme,
            ))
        }
        DialogKind::Help => {
            let d = view
                .as_any_mut()
                .downcast_mut::<HelpDialog>()
                .expect("kind");
            let unread_faults = if app.has_unread_notice_fault() {
                app.unread_notices()
            } else {
                0
            };
            Some(crate::shell::overlays::draw_help_modal(
                frame,
                area,
                app.current_scene(),
                Some(ctx.viewed_session_id),
                unread_faults,
                &app.key_overrides,
                &app.surface_overrides,
                &mut d.scroll,
                &mut d.actions,
                ctx.theme,
            ))
        }
        DialogKind::Notices => {
            let d = view
                .as_any_mut()
                .downcast_mut::<NoticesDialog>()
                .expect("kind");
            Some(crate::shell::overlays::draw_notices_modal(
                frame,
                area,
                &app.notice_lines().iter().cloned().collect::<Vec<_>>(),
                d,
                ctx.theme,
            ))
        }
    }
}
