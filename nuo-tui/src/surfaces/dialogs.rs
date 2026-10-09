//! Encapsulated dialog entities (ADR-0035, `[INV-SURFACE-01]`).
//!
//! Every floating dialog owns its own presentation state — selection cursor,
//! body scroll, follow mode, embedded text input, and sub-layer flags. No
//! dialog state is shared between dialogs or aliased onto `App` scratchpad
//! fields, and no dialog borrows the thread composer line.
//!
//! The [`DialogView`] contract binds each entity to a [`DialogKind`] and its
//! [`DialogScope`] domain; the [`Dialogs`] registry is owned by the surface
//! router and is the single source of truth for dialog state. Server-fed model
//! data (provider snapshots, session context, reports) stays on `App` as
//! read-only model, never as dialog scratchpad.

#![allow(dead_code)]

use std::any::Any;
use std::collections::HashSet;

use nuotc::{Frame, Rect};

use crate::input::InputAction;
use crate::model::layout::LayoutMap;
use crate::model::selection::SelectionState;
use crate::primitives::{ContentModalSpec, FixedModalSpec, ModalSpec};
use crate::render::Theme;
use crate::surfaces::{DialogKind, DialogScope};

/// An embedded single-line text field owned by a dialog. Replaces the old
/// practice of borrowing the thread composer line for a dialog's
/// filter/query (`[INV-SURFACE-01]`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextInput {
    /// The live text.
    pub text: String,
    /// Caret position as a byte offset into `text`.
    pub cursor: usize,
}

impl TextInput {
    /// Clear the field and reset the caret.
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Whether the field is empty.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The trimmed query, for fuzzy matching.
    pub fn query(&self) -> &str {
        self.text.trim()
    }

    /// Move the caret to the end of the text.
    pub fn set_cursor_end(&mut self) {
        self.cursor = self.text.len();
    }
}

/// The result of offering a key to a dialog entity (`[INV-SURFACE-01]`).
#[derive(Debug)]
pub enum DialogOutcome {
    /// The entity handled the input.
    Consumed,
    /// The entity does not own this input; the event loop continues.
    Unhandled,
    /// The entity asks to be dismissed.
    Dismiss,
    /// The entity hands its surface to a successor view.
    SwitchTo(Box<dyn DialogView>),
}

/// The immutable environment injected into [`DialogView::render`]: server-fed
/// model data (read-only) plus the per-frame layout/selection context. Dialogs
/// draw from their own state plus these injected views — never from the
/// composer.
pub struct DialogRenderCtx<'a> {
    pub app: &'a crate::App,
    pub layout_map: &'a mut LayoutMap,
    pub selection: &'a SelectionState,
    pub theme: &'a Theme,
    pub spinner_phase: usize,
    pub viewed_session_id: &'a str,
    pub startup_picker: bool,
    pub input_rect: Option<Rect>,
    pub activity_height: u16,
    pub overlay_owns_caret: bool,
}

/// The presentation contract every floating dialog implements.
pub trait DialogView: std::fmt::Debug + Send + 'static {
    /// This dialog's identity.
    fn kind(&self) -> DialogKind;

    /// This dialog's identity as a compile-time constant, for generic
    /// accessors that must resolve an entity by its concrete type.
    fn kind_static() -> DialogKind
    where
        Self: Sized;

    /// This dialog's ownership domain. Defaults to the static classification
    /// on [`DialogKind::scope`] so the matrix has exactly one definition.
    fn scope(&self) -> DialogScope {
        self.kind().scope()
    }

    /// Visual layout constraints for modal positioning.
    fn layout_spec(&self) -> ModalSpec {
        modal_spec_for(self.kind())
    }

    /// Self-contained input handling: consumes a normalized action and
    /// produces an outcome. Semantic App effects are emitted by the entity;
    /// the event loop applies the outcome.
    fn handle_input(
        &mut self,
        action: &InputAction,
        app: &mut crate::App,
        viewed_session_id: &str,
    ) -> DialogOutcome {
        let _ = (action, app, viewed_session_id);
        DialogOutcome::Unhandled
    }

    /// Autonomous rendering: draws entirely from internal state plus the
    /// injected immutable [`DialogRenderCtx`].
    fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        ctx: &mut DialogRenderCtx<'_>,
    ) -> Option<Rect> {
        let _ = (frame, area, ctx);
        None
    }

    /// The dialog's selection cursor.
    fn nav_index(&self) -> usize;
    /// Set the dialog's selection cursor.
    fn set_nav_index(&mut self, value: usize);
    /// The dialog's scroll offset and (optional) follow flag, for wheel/page
    /// routing.
    fn nav_fields(&mut self) -> (&mut usize, Option<&mut bool>);
    /// Whether the in-dialog key-reference sub-layer is open.
    fn keys_open(&self) -> bool;
    /// Open or close the in-dialog key-reference sub-layer.
    fn set_keys_open(&mut self, open: bool);
    /// The key-reference sub-layer's scroll offset.
    fn keys_scroll_mut(&mut self) -> &mut usize;

    /// Deterministic dismissal hook, executed by the overlay stack on pop and
    /// on scene/session unwinding (`[INV-SURFACE-04]`).
    fn on_dismiss(&mut self) {}

    /// Clear all state back to its first-open value. Used for ephemeral
    /// dismissal and for session-scoped domain isolation (`[INV-SURFACE-05]`).
    fn reset(&mut self);

    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn into_any(self: Box<Self>) -> Box<dyn Any>;

    /// Clone into a boxed trait object. Used to leave a read-consistent
    /// placeholder in a stack entry while the live entity is rendered/input.
    fn clone_box(&self) -> Box<dyn DialogView>;
}

/// The canonical [`ModalSpec`] geometry for a dialog kind.
pub fn modal_spec_for(kind: DialogKind) -> ModalSpec {
    match kind {
        DialogKind::Connections | DialogKind::Models => FixedModalSpec::PROVIDER.modal_spec(),
        DialogKind::Threads => FixedModalSpec::SESSIONS.modal_spec(),
        DialogKind::Tools | DialogKind::SessionTree => ContentModalSpec::TOOLS.modal_spec(),
        DialogKind::Mcp => ContentModalSpec::MCP.modal_spec(),
        DialogKind::Queue => ContentModalSpec::QUEUE.modal_spec(),
        DialogKind::Asides => ContentModalSpec::BTW.modal_spec(),
        DialogKind::SessionStats => ContentModalSpec::SESSION_STATS.modal_spec(),
        DialogKind::SessionTrace => ContentModalSpec::SESSION_TRACE.modal_spec(),
        DialogKind::UsageStats | DialogKind::Quotas => ContentModalSpec::USAGE_STATS.modal_spec(),
        DialogKind::Permissions => ContentModalSpec::PERMISSIONS.modal_spec(),
        DialogKind::Skills => ContentModalSpec::SKILLS.modal_spec(),
        // The history panel is a composer-anchored dropdown and the switcher
        // is a command palette: neither uses a centered modal footprint.
        DialogKind::HistorySearch | DialogKind::Switcher => ContentModalSpec::TOOLS.modal_spec(),
    }
}

/// Per-dialog teardown run on dismissal: the entity's own hook plus clearing
/// its embedded search field, so a dismissed picker never leaves a stale query
/// behind (`[INV-SURFACE-01]`).
#[allow(clippy::expect_used)]
pub fn dismiss_cleanup(view: &mut dyn DialogView) {
    match view.kind() {
        DialogKind::Models => {
            let d = view.as_any_mut().downcast_mut::<ModelsDialog>().expect("kind");
            d.search = false;
            d.query.clear();
        }
        DialogKind::Connections => {
            let d = view
                .as_any_mut()
                .downcast_mut::<ConnectionsDialog>()
                .expect("kind");
            d.search = false;
            d.query.clear();
        }
        DialogKind::HistorySearch => {
            let d = view
                .as_any_mut()
                .downcast_mut::<HistorySearchDialog>()
                .expect("kind");
            d.search = false;
            d.query.clear();
            d.index = 0;
            d.scroll = 0;
            d.follow = true;
        }
        _ => {}
    }
}

/// Render one dialog entity from its own state plus the injected immutable
/// context (`DialogView::render`). The entity's fields are the only mutable
/// dialog state; `ctx` carries read-only server model data and layout context.
#[allow(clippy::expect_used)]
fn render_dialog(
    view: &mut dyn DialogView,
    frame: &mut Frame,
    area: Rect,
    ctx: &mut DialogRenderCtx<'_>,
) -> Option<Rect> {
    let _ = area;
    let app = ctx.app;
    match view.kind() {
        DialogKind::Tools => {
            let d = view.as_any_mut().downcast_mut::<ToolsDialog>().expect("kind");
            Some(crate::overlays::draw_tools_modal(
                frame,
                app.session_context.as_ref(),
                d.index,
                &mut d.scroll,
                d.follow,
                ctx.theme,
            ))
        }
        DialogKind::Mcp => {
            let d = view.as_any_mut().downcast_mut::<McpDialog>().expect("kind");
            Some(crate::overlays::draw_mcp_modal(
                frame,
                app.session_context.as_ref(),
                d.index,
                &mut d.scroll,
                d.follow,
                ctx.theme,
            ))
        }
        DialogKind::Skills => {
            let d = view.as_any_mut().downcast_mut::<SkillsDialog>().expect("kind");
            Some(crate::overlays::draw_skills_modal(
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
            Some(crate::overlays::draw_permissions_manager(
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
            Some(crate::overlays::draw_usage_stats_modal(
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
            Some(crate::overlays::draw_quotas_modal(
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
        DialogKind::SessionStats => {
            let d = view
                .as_any_mut()
                .downcast_mut::<SessionStatsDialog>()
                .expect("kind");
            let report = app.token_source_report(ctx.viewed_session_id);
            let loading = app.token_ledger.is_none() && report.is_none();
            let report = report.unwrap_or_default();
            Some(crate::overlays::draw_session_stats_modal(
                frame,
                &report,
                crate::render::ContextUsageProps {
                    snapshot: app.context_tokens,
                    window_tokens: Some(app.active_model_context_window()),
                    draft_content_tokens: nuo_wire::count_tokens(&app.input),
                    draft_tokens: nuo_wire::estimate_draft_tokens(&app.input),
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
            Some(crate::overlays::draw_session_trace_modal(
                frame,
                &report,
                crate::render::ContextUsageProps {
                    snapshot: app.context_tokens,
                    window_tokens: Some(app.active_model_context_window()),
                    draft_content_tokens: nuo_wire::count_tokens(&app.input),
                    draft_tokens: nuo_wire::estimate_draft_tokens(&app.input),
                },
                d.index
                    .min(crate::render::telemetry_round_count(&report).saturating_sub(1)),
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
            let d = view.as_any_mut().downcast_mut::<QueueDialog>().expect("kind");
            let items: Vec<crate::render::QueueItemProps> = app
                .pending_dispatch
                .iter()
                .filter(|item| item.session_id == ctx.viewed_session_id)
                .map(|item| crate::render::QueueItemProps {
                    queued_at_ms: item.queued_at_ms,
                    text: item.text.clone(),
                })
                .collect();
            let blocked = app.pending_count(ctx.viewed_session_id) > 0
                && app.is_queue_blocked(ctx.viewed_session_id);
            Some(crate::overlays::draw_queue_modal(
                frame,
                crate::render::QueueModalProps {
                    items: &items,
                    blocked,
                },
                d.index,
                &mut d.scroll,
                d.follow,
                ctx.theme,
            ))
        }
        DialogKind::Asides => {
            let d = view.as_any_mut().downcast_mut::<AsidesDialog>().expect("kind");
            let running: Vec<bool> = app
                .btw_list
                .iter()
                .map(|row| app.running_sessions.contains(row.id.as_str()))
                .collect();
            Some(crate::overlays::draw_btw_modal(
                frame,
                crate::render::BtwModalProps {
                    asides: &app.btw_list,
                    running: &running,
                    active_id: app.side_session_id.as_deref(),
                },
                d.index,
                &mut d.scroll,
                d.follow,
                ctx.theme,
                ctx.selection,
                ctx.layout_map,
            ))
        }
        DialogKind::Threads => {
            let d = view.as_any_mut().downcast_mut::<ThreadsDialog>().expect("kind");
            let projected_count = crate::overlays::session::project_session_rows(
                &app.sessions_overview,
                Some(&d.expanded),
            )
            .len();
            Some(crate::overlays::draw_sessions_modal(
                frame,
                crate::overlays::session::SessionsModalProps {
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
            Some(crate::overlays::draw_tree_modal(
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
            Some(crate::overlays::draw_connections_modal(
                frame,
                ctx.layout_map,
                crate::overlays::provider::connections::ConnectionsModalProps {
                    providers: &providers,
                    current_provider: &app.current_provider,
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
                    connection_models_expanded: d.models_expanded,
                    connection_usages: Some(&app.connection_usages),
                },
                ctx.theme,
                ctx.selection,
            ))
        }
        DialogKind::Models => {
            let d = view.as_any_mut().downcast_mut::<ModelsDialog>().expect("kind");
            let models = app.models_flat_filtered();
            Some(crate::overlays::draw_models_modal(
                frame,
                crate::overlays::provider::models::ModelsModalProps {
                    models: &models,
                    current_provider: &app.current_provider,
                    current_model: &app.current_model,
                    modal_index: d.index,
                    query: &d.query.text,
                    cursor_position: d.query.cursor,
                    scroll: &mut d.scroll,
                    follow_selection: d.follow,
                    search: d.search,
                    show_caret: ctx.overlay_owns_caret,
                    refreshing: d.refreshing,
                    spinner_phase: ctx.spinner_phase,
                },
                ctx.theme,
            ))
        }
        DialogKind::HistorySearch => {
            let d = view
                .as_any_mut()
                .downcast_mut::<HistorySearchDialog>()
                .expect("kind");
            let input_rect = ctx.input_rect?;
            let ranked = app.history_rows();
            crate::overlays::draw_history_panel(
                frame,
                crate::overlays::history::HistoryPanelProps {
                    history: &app.input_history,
                    ranked: &ranked,
                    modal_index: d.index,
                    scroll: &mut d.scroll,
                    follow_selection: d.follow,
                    input_rect,
                    activity_height: ctx.activity_height,
                    query: &d.query.text,
                    cursor_position: d.query.cursor,
                    show_caret: ctx.overlay_owns_caret,
                },
                ctx.theme,
            )
        }
        DialogKind::Switcher => {
            let d = view
                .as_any_mut()
                .downcast_mut::<SwitcherDialog>()
                .expect("kind");
            let app_ctx = crate::keymap::AppContext {
                has_overlay: true,
                active_dialog: app.surfaces.underlying_dialog().or_else(|| app.active_dialog()),
                is_responding: app.viewed_chrome().responding,
                has_selection: !matches!(app.selection, SelectionState::None),
                has_running_task: app.viewed_chrome().responding,
                queue_count: app.pending_dispatch.len(),
                has_session: app.has_session(),
                scene: app.current_scene(),
            };
            let entries = crate::overlays::command_palette::filter_palette_commands(
                &d.query.text,
                &app.command_catalog,
                &app.recent_commands,
                &app_ctx,
            );
            let show_caret = app.caret_visible() && app.caret_owner() == crate::CaretOwner::Overlay;
            Some(crate::overlays::draw_command_palette(
                frame,
                crate::overlays::command_palette::CommandPaletteProps {
                    query: &d.query.text,
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
    }
}

/// Handle a local navigation/scroll action for one dialog entity
/// (`DialogView::handle_input`). `ModalUp`/`ModalDown` are the generic ↑/↓
/// arrows; `SessionSelect` is the list-navigation family. Returns `Unhandled`
/// for actions the entity does not own, so the event loop keeps dispatching
/// semantic commands.
#[allow(clippy::expect_used)]
fn input_dialog(
    view: &mut dyn DialogView,
    action: &InputAction,
    app: &mut crate::App,
    viewed_session_id: &str,
) -> DialogOutcome {
    use InputAction as A;
    // `(forward, is_modal_arrow)`.
    let (forward, arrow) = match action {
        A::ModalUp => (false, true),
        A::ModalDown => (true, true),
        A::SessionSelect { forward } => (*forward, false),
        _ => return DialogOutcome::Unhandled,
    };

    match view.kind() {
        DialogKind::Tools | DialogKind::Mcp | DialogKind::Skills => {
            if arrow {
                return DialogOutcome::Unhandled;
            }
            let count = match view.kind() {
                DialogKind::Mcp => app.session_context.as_ref().map_or(0, |s| s.mcp.len()),
                DialogKind::Skills => app.session_context.as_ref().map_or(0, |s| s.skills.len()),
                _ => app.session_tools_len(),
            };
            if count == 0 {
                return DialogOutcome::Unhandled;
            }
            match view.kind() {
                DialogKind::Tools => {
                    let e = view.as_any_mut().downcast_mut::<ToolsDialog>().expect("kind");
                    e.index = rotate(e.index, count, forward);
                    e.follow = true;
                }
                DialogKind::Mcp => {
                    let e = view.as_any_mut().downcast_mut::<McpDialog>().expect("kind");
                    e.index = rotate(e.index, count, forward);
                    e.follow = true;
                }
                _ => {
                    let e = view.as_any_mut().downcast_mut::<SkillsDialog>().expect("kind");
                    e.index = rotate(e.index, count, forward);
                    e.follow = true;
                }
            }
            DialogOutcome::Consumed
        }
        DialogKind::Permissions => {
            let count = app
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
            let e = view.as_any_mut().downcast_mut::<ModelsDialog>().expect("kind");
            let count = app.picker_row_count();
            if count == 0 {
                return DialogOutcome::Unhandled;
            }
            e.index = rotate(e.index, count, forward);
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
            e.index = rotate(e.index, count, forward);
            e.follow = true;
            DialogOutcome::Consumed
        }
        DialogKind::Threads => {
            let e = view.as_any_mut().downcast_mut::<ThreadsDialog>().expect("kind");
            let count = crate::overlays::session::project_session_rows(
                &app.sessions_overview,
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
            let count = crate::overlays::tree::flatten_tree(&app.session_tree).len();
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
        DialogKind::SessionStats => {
            let e = view
                .as_any_mut()
                .downcast_mut::<SessionStatsDialog>()
                .expect("kind");
            if forward {
                e.scroll = e.scroll.saturating_add(1);
            } else {
                e.scroll = e.scroll.saturating_sub(1);
            }
            DialogOutcome::Consumed
        }
        DialogKind::SessionTrace => {
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
                        .map(|r| crate::overlays::telemetry_round_count(r).saturating_sub(1))
                        .unwrap_or(0),
                );
                let count = report
                    .as_ref()
                    .map(|r| crate::overlays::telemetry_attempt_count(r, round_index))
                    .unwrap_or(0)
                    .max(1);
                e.turn_cursor = rotate(e.turn_cursor, count, forward);
            } else {
                let count = app
                    .token_source_report(viewed_session_id)
                    .map(|r| crate::overlays::telemetry_round_count(&r))
                    .unwrap_or(0)
                    .max(1);
                e.index = rotate(e.index, count, forward);
            }
            DialogOutcome::Consumed
        }
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
            let e = view.as_any_mut().downcast_mut::<QueueDialog>().expect("kind");
            if arrow {
                if forward {
                    e.scroll = e.scroll.saturating_add(1);
                } else {
                    e.scroll = e.scroll.saturating_sub(1);
                }
                e.follow = false;
            } else {
                let count = app
                    .pending_dispatch
                    .iter()
                    .filter(|item| item.session_id == viewed_session_id)
                    .count();
                if count == 0 {
                    return DialogOutcome::Unhandled;
                }
                e.index = rotate(e.index, count, forward);
                e.follow = true;
            }
            DialogOutcome::Consumed
        }
        DialogKind::Asides => {
            let e = view.as_any_mut().downcast_mut::<AsidesDialog>().expect("kind");
            if arrow {
                if forward {
                    e.scroll = e.scroll.saturating_add(1);
                } else {
                    e.scroll = e.scroll.saturating_sub(1);
                }
                e.follow = false;
            } else {
                let count = app.btw_list.len();
                if count == 0 {
                    return DialogOutcome::Unhandled;
                }
                e.index = rotate(e.index, count, forward);
                e.follow = true;
            }
            DialogOutcome::Consumed
        }
        DialogKind::Switcher => {
            let e = view.as_any_mut().downcast_mut::<SwitcherDialog>().expect("kind");
            let count = crate::overlays::command_palette::filter_palette_commands(
                &e.query.text,
                &app.command_catalog,
                &app.recent_commands,
                &switcher_app_ctx(app),
            )
            .len();
            if count == 0 {
                return DialogOutcome::Unhandled;
            }
            e.selected = rotate(e.selected, count, forward);
            DialogOutcome::Consumed
        }
    }
}

fn rotate(cur: usize, count: usize, forward: bool) -> usize {
    if count == 0 {
        0
    } else if forward {
        (cur + 1) % count
    } else if cur == 0 {
        count - 1
    } else {
        cur - 1
    }
}

fn switcher_app_ctx(app: &crate::App) -> crate::keymap::AppContext {
    crate::keymap::AppContext {
        has_overlay: true,
        active_dialog: app.surfaces.underlying_dialog().or_else(|| app.active_dialog()),
        is_responding: app.viewed_chrome().responding,
        has_selection: !matches!(app.selection, SelectionState::None),
        has_running_task: app.viewed_chrome().responding,
        queue_count: app.pending_dispatch.len(),
        has_session: app.has_session(),
        scene: app.current_scene(),
    }
}

/// Define a dialog entity with the flattened navigation fields shared by every
/// dialog, plus entity-specific fields, and the `DialogView` boilerplate.
macro_rules! dialog_entity {
    (
        $name:ident, $kind:ident,
        { $( $(#[$meta:meta])* $field:ident : $ty:ty = $default:expr ),* $(,)? }
    ) => {
        #[derive(Debug, Clone)]
        pub struct $name {
            /// Selection cursor over the dialog's projected rows.
            pub index: usize,
            /// Body scroll offset.
            pub scroll: usize,
            /// Whether body scroll follows the selection.
            pub follow: bool,
            /// Whether the in-dialog key-reference sub-layer is open.
            pub keys_open: bool,
            /// Scroll offset of the key-reference sub-layer.
            pub keys_scroll: usize,
            $( $(#[$meta])* pub $field: $ty, )*
        }

        impl Default for $name {
            fn default() -> Self {
                Self {
                    index: 0,
                    scroll: 0,
                    follow: true,
                    keys_open: false,
                    keys_scroll: 0,
                    $($field: $default,)*
                }
            }
        }

        impl DialogView for $name {
            fn kind(&self) -> DialogKind {
                DialogKind::$kind
            }
            fn kind_static() -> DialogKind {
                DialogKind::$kind
            }
            fn handle_input(
                &mut self,
                action: &InputAction,
                app: &mut crate::App,
                viewed_session_id: &str,
            ) -> DialogOutcome {
                input_dialog(self, action, app, viewed_session_id)
            }
            fn render(
                &mut self,
                frame: &mut Frame,
                area: Rect,
                ctx: &mut DialogRenderCtx<'_>,
            ) -> Option<Rect> {
                render_dialog(self, frame, area, ctx)
            }
            fn nav_index(&self) -> usize {
                self.index
            }
            fn set_nav_index(&mut self, value: usize) {
                self.index = value;
            }
            fn nav_fields(&mut self) -> (&mut usize, Option<&mut bool>) {
                (&mut self.scroll, Some(&mut self.follow))
            }
            fn keys_open(&self) -> bool {
                self.keys_open
            }
            fn set_keys_open(&mut self, open: bool) {
                self.keys_open = open;
                if !open {
                    self.keys_scroll = 0;
                }
            }
            fn keys_scroll_mut(&mut self) -> &mut usize {
                &mut self.keys_scroll
            }
            fn reset(&mut self) {
                *self = Self::default();
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
            fn as_any_mut(&mut self) -> &mut dyn Any {
                self
            }
            fn into_any(self: Box<Self>) -> Box<dyn Any> {
                self
            }
            fn clone_box(&self) -> Box<dyn DialogView> {
                Box::new(self.clone())
            }
        }
    };
}

dialog_entity!(ToolsDialog, Tools, {});
dialog_entity!(McpDialog, Mcp, {});

dialog_entity!(SkillsDialog, Skills, {
    /// Row whose detail block is expanded, if any.
    expanded: Option<usize> = None,
});

dialog_entity!(PermissionsDialog, Permissions, {});
dialog_entity!(UsageStatsDialog, UsageStats, {});
dialog_entity!(QuotasDialog, Quotas, {
    /// Row whose detail block is expanded, if any.
    expanded: Option<usize> = None,
});

dialog_entity!(SessionStatsDialog, SessionStats, {});

dialog_entity!(SessionTraceDialog, SessionTrace, {
    /// `true` when drilled into one round's turns (L2).
    detail: bool = false,
    /// `Some((round, attempt))` when drilled into an attempt inspector (L3).
    turn: Option<(u32, u32)> = None,
    /// Selected turn index in the L2 turns table.
    turn_cursor: usize = 0,
});

dialog_entity!(AsidesDialog, Asides, {});
dialog_entity!(QueueDialog, Queue, {});

dialog_entity!(ModelsDialog, Models, {
    /// Whether the search sub-layer owns the embedded input.
    search: bool = false,
    /// Embedded filter query (self-contained; never the composer line).
    query: TextInput = TextInput::default(),
    /// Whether a catalog refresh is in flight.
    refreshing: bool = false,
});

dialog_entity!(ConnectionsDialog, Connections, {
    /// Whether the search sub-layer owns the embedded input.
    search: bool = false,
    /// Embedded filter query (self-contained; never the composer line).
    query: TextInput = TextInput::default(),
    /// Whether a catalog refresh is in flight.
    refreshing: bool = false,
    /// `true` while the connection-info sub-view is open.
    info_detail: bool = false,
    /// `true` when the info sub-view was opened standalone (not drilled in).
    info_standalone: bool = false,
    /// Body scroll offset of the connection-info sub-view.
    info_scroll: usize = 0,
    /// Whether the served-models list is expanded in the info sub-view.
    models_expanded: bool = false,
});

dialog_entity!(HistorySearchDialog, HistorySearch, {
    /// Whether the search sub-layer is active.
    search: bool = false,
    /// Embedded fuzzy query (self-contained; never the composer line).
    query: TextInput = TextInput::default(),
});

dialog_entity!(ThreadsDialog, Threads, {
    /// `true` while the session-info sub-view is open.
    info_detail: bool = false,
    /// Body scroll offset of the session-info sub-view.
    info_scroll: usize = 0,
    /// Whether the sessions list round-trip is in flight.
    loading: bool = true,
    /// Expanded trunk session ids in the hierarchical picker.
    expanded: HashSet<String> = HashSet::new(),
});

/// Backward-compatible alias for [`ThreadsDialog`].
pub type SessionsDialog = ThreadsDialog;

dialog_entity!(SessionTreeDialog, SessionTree, {});

dialog_entity!(SwitcherDialog, Switcher, {
    /// Live fuzzy query.
    query: TextInput = TextInput::default(),
    /// Selected row in the palette.
    selected: usize = 0,
});

impl SwitcherDialog {
    /// The switcher is ephemeral: opening it always starts from a clean slate.
    pub fn begin(&mut self) {
        self.query.clear();
        self.selected = 0;
        self.scroll = 0;
    }
}

/// A snapshot of every session-scoped dialog entity, archived under a
/// [`SessionId`](crate::surfaces) so returning to a session reinstates exactly
/// the modal state the user left behind (`[INV-SURFACE-05]`).
#[derive(Debug, Clone, Default)]
pub struct SessionSnapshot {
    pub tools: ToolsDialog,
    pub mcp: McpDialog,
    pub skills: SkillsDialog,
    pub permissions: PermissionsDialog,
    pub session_stats: SessionStatsDialog,
    pub session_trace: SessionTraceDialog,
    pub asides: AsidesDialog,
    pub queue: QueueDialog,
    pub session_tree: SessionTreeDialog,
}

/// The single registry of every dialog entity, owned by the surface router.
///
/// Each field is a distinct, encapsulated entity: dialogs never share state.
/// Global entities persist across sessions untouched; session-scoped entities
/// are archived per session so a session switch neither loses nor leaks them.
#[derive(Debug, Default)]
pub struct Dialogs {
    pub tools: ToolsDialog,
    pub mcp: McpDialog,
    pub skills: SkillsDialog,
    pub permissions: PermissionsDialog,
    pub usage_stats: UsageStatsDialog,
    pub quotas: QuotasDialog,
    pub session_stats: SessionStatsDialog,
    pub session_trace: SessionTraceDialog,
    pub asides: AsidesDialog,
    pub models: ModelsDialog,
    pub connections: ConnectionsDialog,
    pub history_search: HistorySearchDialog,
    pub queue: QueueDialog,
    pub threads: ThreadsDialog,
    pub session_tree: SessionTreeDialog,
    pub switcher: SwitcherDialog,
    /// Per-session archives of the session-scoped entities.
    session_archive: std::collections::HashMap<String, SessionSnapshot>,
    /// The session whose live state the session-scoped entities currently
    /// hold (empty before the first session is bound).
    current_session: String,
}

impl Dialogs {
    /// Archive the live session-scoped state under `session_id`.
    pub fn archive_session(&mut self, session_id: &str) {
        if session_id.is_empty() {
            return;
        }
        self.session_archive
            .insert(session_id.to_string(), self.session_snapshot());
    }

    /// Reinstate `session_id`'s archived session-scoped state, or reset the
    /// session-scoped entities to first-open when no archive exists.
    pub fn restore_session(&mut self, session_id: &str) {
        match self.session_archive.get(session_id).cloned() {
            Some(snapshot) => self.apply_session_snapshot(snapshot),
            None => self.reset_scope(DialogScope::Session),
        }
    }

    /// Transition the session-scoped domain from the current session to
    /// `new_id`: archive the outgoing session, then reinstate the incoming one.
    /// Global dialog state is never touched (`[INV-SURFACE-05]`).
    pub fn switch_session(&mut self, new_id: &str) {
        if self.current_session == new_id {
            return;
        }
        if !self.current_session.is_empty() {
            let outgoing = self.current_session.clone();
            self.archive_session(&outgoing);
        }
        self.restore_session(new_id);
        self.current_session = new_id.to_string();
    }

    fn session_snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            tools: self.tools.clone(),
            mcp: self.mcp.clone(),
            skills: self.skills.clone(),
            permissions: self.permissions.clone(),
            session_stats: self.session_stats.clone(),
            session_trace: self.session_trace.clone(),
            asides: self.asides.clone(),
            queue: self.queue.clone(),
            session_tree: self.session_tree.clone(),
        }
    }

    fn apply_session_snapshot(&mut self, snapshot: SessionSnapshot) {
        self.tools = snapshot.tools;
        self.mcp = snapshot.mcp;
        self.skills = snapshot.skills;
        self.permissions = snapshot.permissions;
        self.session_stats = snapshot.session_stats;
        self.session_trace = snapshot.session_trace;
        self.asides = snapshot.asides;
        self.queue = snapshot.queue;
        self.session_tree = snapshot.session_tree;
    }
}

impl Dialogs {
    /// The entity for `kind`, as the `DialogView` contract.
    pub fn view(&self, kind: DialogKind) -> &dyn DialogView {
        match kind {
            DialogKind::Tools => &self.tools,
            DialogKind::Mcp => &self.mcp,
            DialogKind::Skills => &self.skills,
            DialogKind::Permissions => &self.permissions,
            DialogKind::UsageStats => &self.usage_stats,
            DialogKind::Quotas => &self.quotas,
            DialogKind::SessionStats => &self.session_stats,
            DialogKind::SessionTrace => &self.session_trace,
            DialogKind::Asides => &self.asides,
            DialogKind::Models => &self.models,
            DialogKind::Connections => &self.connections,
            DialogKind::HistorySearch => &self.history_search,
            DialogKind::Queue => &self.queue,
            DialogKind::Threads => &self.threads,
            DialogKind::SessionTree => &self.session_tree,
            DialogKind::Switcher => &self.switcher,
        }
    }

    /// The mutable entity for `kind`.
    pub fn view_mut(&mut self, kind: DialogKind) -> &mut dyn DialogView {
        match kind {
            DialogKind::Tools => &mut self.tools,
            DialogKind::Mcp => &mut self.mcp,
            DialogKind::Skills => &mut self.skills,
            DialogKind::Permissions => &mut self.permissions,
            DialogKind::UsageStats => &mut self.usage_stats,
            DialogKind::Quotas => &mut self.quotas,
            DialogKind::SessionStats => &mut self.session_stats,
            DialogKind::SessionTrace => &mut self.session_trace,
            DialogKind::Asides => &mut self.asides,
            DialogKind::Models => &mut self.models,
            DialogKind::Connections => &mut self.connections,
            DialogKind::HistorySearch => &mut self.history_search,
            DialogKind::Queue => &mut self.queue,
            DialogKind::Threads => &mut self.threads,
            DialogKind::SessionTree => &mut self.session_tree,
            DialogKind::Switcher => &mut self.switcher,
        }
    }

    /// Take the entity for `kind` out of the registry (leaving a fresh
    /// placeholder), so the caller can invoke its `render`/`handle_input`
    /// against a disjoint `&App` borrow. Must be paired with [`Self::put`].
    #[allow(clippy::expect_used)]
    pub fn take(&mut self, kind: DialogKind) -> Box<dyn DialogView> {
        // Move the entity out, leaving a `Default` placeholder; the stack owns
        // it while open and returns it via `put` on dismissal. The
        // render-time read-consistency clone lives in the stack entry
        // (`SurfaceRouter::take_active_view`).
        match kind {
            DialogKind::Tools => Box::new(std::mem::take(&mut self.tools)),
            DialogKind::Mcp => Box::new(std::mem::take(&mut self.mcp)),
            DialogKind::Skills => Box::new(std::mem::take(&mut self.skills)),
            DialogKind::Permissions => Box::new(std::mem::take(&mut self.permissions)),
            DialogKind::UsageStats => Box::new(std::mem::take(&mut self.usage_stats)),
            DialogKind::Quotas => Box::new(std::mem::take(&mut self.quotas)),
            DialogKind::SessionStats => Box::new(std::mem::take(&mut self.session_stats)),
            DialogKind::SessionTrace => Box::new(std::mem::take(&mut self.session_trace)),
            DialogKind::Asides => Box::new(std::mem::take(&mut self.asides)),
            DialogKind::Models => Box::new(std::mem::take(&mut self.models)),
            DialogKind::Connections => Box::new(std::mem::take(&mut self.connections)),
            DialogKind::HistorySearch => Box::new(std::mem::take(&mut self.history_search)),
            DialogKind::Queue => Box::new(std::mem::take(&mut self.queue)),
            DialogKind::Threads => Box::new(std::mem::take(&mut self.threads)),
            DialogKind::SessionTree => Box::new(std::mem::take(&mut self.session_tree)),
            DialogKind::Switcher => Box::new(std::mem::take(&mut self.switcher)),
        }
    }

    /// Put an entity taken with [`Self::take`] back into the registry.
    #[allow(clippy::expect_used)]
    pub fn put(&mut self, kind: DialogKind, view: Box<dyn DialogView>) {
        match kind {
            DialogKind::Tools => self.tools = *view.into_any().downcast::<ToolsDialog>().expect("kind"),
            DialogKind::Mcp => self.mcp = *view.into_any().downcast::<McpDialog>().expect("kind"),
            DialogKind::Skills => {
                self.skills = *view.into_any().downcast::<SkillsDialog>().expect("kind")
            }
            DialogKind::Permissions => {
                self.permissions = *view.into_any().downcast::<PermissionsDialog>().expect("kind")
            }
            DialogKind::UsageStats => {
                self.usage_stats = *view.into_any().downcast::<UsageStatsDialog>().expect("kind")
            }
            DialogKind::Quotas => {
                self.quotas = *view.into_any().downcast::<QuotasDialog>().expect("kind")
            }
            DialogKind::SessionStats => {
                self.session_stats =
                    *view.into_any().downcast::<SessionStatsDialog>().expect("kind")
            }
            DialogKind::SessionTrace => {
                self.session_trace =
                    *view.into_any().downcast::<SessionTraceDialog>().expect("kind")
            }
            DialogKind::Asides => {
                self.asides = *view.into_any().downcast::<AsidesDialog>().expect("kind")
            }
            DialogKind::Models => self.models = *view.into_any().downcast::<ModelsDialog>().expect("kind"),
            DialogKind::Connections => {
                self.connections = *view.into_any().downcast::<ConnectionsDialog>().expect("kind")
            }
            DialogKind::HistorySearch => {
                self.history_search =
                    *view.into_any().downcast::<HistorySearchDialog>().expect("kind")
            }
            DialogKind::Queue => self.queue = *view.into_any().downcast::<QueueDialog>().expect("kind"),
            DialogKind::Threads => {
                self.threads = *view.into_any().downcast::<ThreadsDialog>().expect("kind")
            }
            DialogKind::SessionTree => {
                self.session_tree = *view.into_any().downcast::<SessionTreeDialog>().expect("kind")
            }
            DialogKind::Switcher => {
                self.switcher = *view.into_any().downcast::<SwitcherDialog>().expect("kind")
            }
        }
    }

    /// The dialog's selection cursor.
    pub fn index(&self, kind: DialogKind) -> usize {
        match kind {
            DialogKind::Tools => self.tools.index,
            DialogKind::Mcp => self.mcp.index,
            DialogKind::Skills => self.skills.index,
            DialogKind::Permissions => self.permissions.index,
            DialogKind::UsageStats => self.usage_stats.index,
            DialogKind::Quotas => self.quotas.index,
            DialogKind::SessionStats => self.session_stats.index,
            DialogKind::SessionTrace => self.session_trace.index,
            DialogKind::Asides => self.asides.index,
            DialogKind::Models => self.models.index,
            DialogKind::Connections => self.connections.index,
            DialogKind::HistorySearch => self.history_search.index,
            DialogKind::Queue => self.queue.index,
            DialogKind::Threads => self.threads.index,
            DialogKind::SessionTree => self.session_tree.index,
            DialogKind::Switcher => self.switcher.selected,
        }
    }

    /// Set the dialog's selection cursor.
    pub fn set_index(&mut self, kind: DialogKind, value: usize) {
        match kind {
            DialogKind::Tools => self.tools.index = value,
            DialogKind::Mcp => self.mcp.index = value,
            DialogKind::Skills => self.skills.index = value,
            DialogKind::Permissions => self.permissions.index = value,
            DialogKind::UsageStats => self.usage_stats.index = value,
            DialogKind::Quotas => self.quotas.index = value,
            DialogKind::SessionStats => self.session_stats.index = value,
            DialogKind::SessionTrace => self.session_trace.index = value,
            DialogKind::Asides => self.asides.index = value,
            DialogKind::Models => self.models.index = value,
            DialogKind::Connections => self.connections.index = value,
            DialogKind::HistorySearch => self.history_search.index = value,
            DialogKind::Queue => self.queue.index = value,
            DialogKind::Threads => self.threads.index = value,
            DialogKind::SessionTree => self.session_tree.index = value,
            DialogKind::Switcher => self.switcher.selected = value,
        }
    }

    /// Whether the dialog's body scroll follows its selection.
    pub fn follow(&self, kind: DialogKind) -> bool {
        match kind {
            DialogKind::Tools => self.tools.follow,
            DialogKind::Mcp => self.mcp.follow,
            DialogKind::Skills => self.skills.follow,
            DialogKind::Permissions => self.permissions.follow,
            DialogKind::UsageStats => self.usage_stats.follow,
            DialogKind::Quotas => self.quotas.follow,
            DialogKind::SessionStats => self.session_stats.follow,
            DialogKind::SessionTrace => self.session_trace.follow,
            DialogKind::Asides => self.asides.follow,
            DialogKind::Models => self.models.follow,
            DialogKind::Connections => self.connections.follow,
            DialogKind::HistorySearch => self.history_search.follow,
            DialogKind::Queue => self.queue.follow,
            DialogKind::Threads => self.threads.follow,
            DialogKind::SessionTree => self.session_tree.follow,
            DialogKind::Switcher => self.switcher.follow,
        }
    }

    /// Whether the dialog's key-reference sub-layer is open.
    pub fn keys_open(&self, kind: DialogKind) -> bool {
        match kind {
            DialogKind::Tools => self.tools.keys_open,
            DialogKind::Mcp => self.mcp.keys_open,
            DialogKind::Skills => self.skills.keys_open,
            DialogKind::Permissions => self.permissions.keys_open,
            DialogKind::UsageStats => self.usage_stats.keys_open,
            DialogKind::Quotas => self.quotas.keys_open,
            DialogKind::SessionStats => self.session_stats.keys_open,
            DialogKind::SessionTrace => self.session_trace.keys_open,
            DialogKind::Asides => self.asides.keys_open,
            DialogKind::Models => self.models.keys_open,
            DialogKind::Connections => self.connections.keys_open,
            DialogKind::HistorySearch => self.history_search.keys_open,
            DialogKind::Queue => self.queue.keys_open,
            DialogKind::Threads => self.threads.keys_open,
            DialogKind::SessionTree => self.session_tree.keys_open,
            DialogKind::Switcher => self.switcher.keys_open,
        }
    }

    /// Open or close the dialog's key-reference sub-layer.
    pub fn set_keys_open(&mut self, kind: DialogKind, open: bool) {
        // Every entity exposes the same flattened fields; route by kind.
        match kind {
            DialogKind::Tools => self.tools.keys_open = open,
            DialogKind::Mcp => self.mcp.keys_open = open,
            DialogKind::Skills => self.skills.keys_open = open,
            DialogKind::Permissions => self.permissions.keys_open = open,
            DialogKind::UsageStats => self.usage_stats.keys_open = open,
            DialogKind::Quotas => self.quotas.keys_open = open,
            DialogKind::SessionStats => self.session_stats.keys_open = open,
            DialogKind::SessionTrace => self.session_trace.keys_open = open,
            DialogKind::Asides => self.asides.keys_open = open,
            DialogKind::Models => self.models.keys_open = open,
            DialogKind::Connections => self.connections.keys_open = open,
            DialogKind::HistorySearch => self.history_search.keys_open = open,
            DialogKind::Queue => self.queue.keys_open = open,
            DialogKind::Threads => self.threads.keys_open = open,
            DialogKind::SessionTree => self.session_tree.keys_open = open,
            DialogKind::Switcher => self.switcher.keys_open = open,
        }
        if !open {
            *self.keys_scroll_mut(kind) = 0;
        }
    }

    /// Mutable key-reference scroll offset.
    pub fn keys_scroll_mut(&mut self, kind: DialogKind) -> &mut usize {
        match kind {
            DialogKind::Tools => &mut self.tools.keys_scroll,
            DialogKind::Mcp => &mut self.mcp.keys_scroll,
            DialogKind::Skills => &mut self.skills.keys_scroll,
            DialogKind::Permissions => &mut self.permissions.keys_scroll,
            DialogKind::UsageStats => &mut self.usage_stats.keys_scroll,
            DialogKind::Quotas => &mut self.quotas.keys_scroll,
            DialogKind::SessionStats => &mut self.session_stats.keys_scroll,
            DialogKind::SessionTrace => &mut self.session_trace.keys_scroll,
            DialogKind::Asides => &mut self.asides.keys_scroll,
            DialogKind::Models => &mut self.models.keys_scroll,
            DialogKind::Connections => &mut self.connections.keys_scroll,
            DialogKind::HistorySearch => &mut self.history_search.keys_scroll,
            DialogKind::Queue => &mut self.queue.keys_scroll,
            DialogKind::Threads => &mut self.threads.keys_scroll,
            DialogKind::SessionTree => &mut self.session_tree.keys_scroll,
            DialogKind::Switcher => &mut self.switcher.keys_scroll,
        }
    }

    /// Run the dismissal hook for `kind`: the entity's own hook plus the
    /// per-dialog teardown of the embedded search field, so a dismissed
    /// picker never leaves a stale query behind (`[INV-SURFACE-01]`).
    pub fn on_dismiss(&mut self, kind: DialogKind) {
        self.view_mut(kind).on_dismiss();
        dismiss_cleanup(self.view_mut(kind));
    }

    /// Clear one entity's state back to first-open.
    pub fn reset(&mut self, kind: DialogKind) {
        self.view_mut(kind).reset();
    }

    /// Clear every entity whose domain matches `scope`.
    pub fn reset_scope(&mut self, scope: DialogScope) {
        for kind in DialogKind::ALL {
            if kind.scope() == scope {
                self.reset(kind);
            }
        }
        if scope == DialogScope::Global {
            self.switcher.reset();
        }
    }
}


