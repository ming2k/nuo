//! Dialog entity definitions: one struct per floating surface.

use super::input::input_dialog;
use super::render::render_dialog;
use super::*;

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
                app: &mut crate::shell::App,
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

dialog_entity!(ToolsDialog, Tools, {
    /// `true` while the tool-detail sub-view is open.
    info_detail: bool = false,
    /// Body scroll offset of the tool-detail sub-view.
    info_scroll: usize = 0,
});
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

dialog_entity!(ThreadStatsDialog, ThreadStats, {});

dialog_entity!(SessionTraceDialog, SessionTrace, {
    /// `true` when drilled into one round's turns (L2).
    detail: bool = false,
    /// `Some((round, attempt))` when drilled into an attempt inspector (L3).
    turn: Option<(u32, u32)> = None,
    /// Selected turn index in the L2 turns table.
    turn_cursor: usize = 0,
});

dialog_entity!(QueueDialog, Queue, {});

dialog_entity!(ModelsDialog, Models, {
    /// Whether the search sub-layer owns the embedded input.
    search: bool = false,
    /// Embedded filter query (self-contained; never the composer line).
    query: TextInput = TextInput::default(),
    /// Whether a catalog refresh is in flight.
    refreshing: bool = false,
    /// `true` while the model-info sub-view is open.
    info_detail: bool = false,
    /// Body scroll offset of the model-info sub-view.
    info_scroll: usize = 0,
    /// Whether the focused row is currently revealing a cell that does not fit
    /// (`[INV-TUI-MARQUEE-01]`). Written by the draw, read by the event loop:
    /// the animation heartbeat stays alive exactly while something moves and
    /// sleeps the moment nothing does, so an idle list costs no repaints.
    reveal_active: bool = false,
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
});

dialog_entity!(HistorySearchDialog, HistorySearch, {});

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

dialog_entity!(PortShareDialog, PortShare, {
    port: u16 = 0,
    active: bool = false,
    token: Option<String> = None,
    local_path: String = String::new(),
    notice: Option<String> = None,
    loading: bool = false,
});

impl PortShareDialog {
    pub fn begin(&mut self) {
        self.notice = None;
    }
}

dialog_entity!(HelpDialog, Help, {
    /// Cached actions for each row in the rendered help dialog.
    actions: Vec<Option<crate::shell::overlays::help::HelpActionKind>> = Vec::new(),
});

dialog_entity!(NoticesDialog, Notices, {
    /// Whether the focused notice's detail view is expanded.
    detail: bool = false,
    /// Body height of the modal list.
    body_height: u16 = 0,
});
