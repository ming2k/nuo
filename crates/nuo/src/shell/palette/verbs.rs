//! Scene and dialog verbs: the single declaration of what each surface can do.
//!
//! A [`Verb`] pairs a human description with the **exact** [`InputAction`] its
//! keycap produces. Two consumers read these tables: the command palette
//! (which runs the action) and the in-dialog key reference (`?`). The tests in
//! `palette::tests` resolve every keycap through the real router and require it
//! to yield the declared action (RFC-0005 `[INV-PAL-CTX-02]`), so a table row
//! cannot describe a key that does something else.
//!
//! Both lookups are exhaustive `match`es with no wildcard arm
//! (`[INV-PAL-CTX-05]`): a new scene or dialog does not compile until someone
//! decides what the palette says about it.

use crate::shell::focus::FocusPlane;
use crate::shell::input::InputAction;
use crate::shell::keymap::{Availability, DangerLevel, Key, KeyCode, SurfaceVerb};
use crate::shell::surfaces::{DialogKind, SceneKind};

use super::context::PaletteContext;

/// The state a verb needs before it can run. One declaration drives both the
/// palette's availability (with a reason that says how to get there) and the
/// key-state fixture the keycap honesty test resolves under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Requires {
    Nothing,
    /// A round is running in the viewed session.
    Running,
    /// The composer plane owns the keyboard.
    ComposerPlane,
    /// The transcript plane owns the keyboard.
    TranscriptPlane,
    /// A transcript entry is highlighted.
    FocusedEntry,
    /// The Settings detail pane owns focus.
    SettingsDetail,
}

impl Requires {
    pub(crate) fn check(self, ctx: &PaletteContext) -> Availability {
        let met = match self {
            Requires::Nothing => true,
            Requires::Running => ctx.state.is_responding,
            Requires::ComposerPlane => ctx.focus == FocusPlane::Composer,
            Requires::TranscriptPlane => ctx.focus == FocusPlane::Transcript,
            Requires::FocusedEntry => ctx.state.has_focused_entry,
            Requires::SettingsDetail => ctx.settings_detail,
        };
        if met {
            Availability::Available
        } else {
            Availability::Unavailable(self.reason())
        }
    }

    /// Why the verb cannot run, phrased as what to do about it.
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Requires::Nothing => "",
            Requires::Running => "only while a round runs",
            Requires::ComposerPlane => "focus the composer first (Shift-Tab)",
            Requires::TranscriptPlane => "focus the transcript first (Shift-Tab)",
            Requires::FocusedEntry => "highlight an entry first (Ctrl-p)",
            Requires::SettingsDetail => "focus the detail pane first",
        }
    }
}

/// One verb of one surface.
#[derive(Debug)]
pub(crate) struct Verb {
    /// Stable entry key (`diff.next-file`).
    pub key: &'static str,
    pub title: &'static str,
    pub detail: &'static str,
    pub keywords: &'static str,
    /// The canonical chord, when the verb has one.
    pub keycap: Option<Key>,
    /// The user-remappable verb behind `keycap`, so a remap is what is shown.
    pub remap: Option<SurfaceVerb>,
    pub requires: Requires,
    pub danger: DangerLevel,
    pub action: InputAction,
}

impl Verb {
    /// The chord that fires this verb under the user's remaps.
    pub(crate) fn effective_keycap(&self, ctx: &PaletteContext) -> Option<Key> {
        match self.remap {
            Some(verb) => Some(ctx.surface_keys.effective_binding(verb)),
            None => self.keycap,
        }
    }
}

const fn verb(
    key: &'static str,
    title: &'static str,
    detail: &'static str,
    keycap: Key,
    action: InputAction,
) -> Verb {
    Verb {
        key,
        title,
        detail,
        keywords: "",
        keycap: Some(keycap),
        remap: None,
        requires: Requires::Nothing,
        danger: DangerLevel::Safe,
        action,
    }
}

impl Verb {
    const fn requires(mut self, requires: Requires) -> Self {
        self.requires = requires;
        self
    }
    const fn remap(mut self, remap: SurfaceVerb) -> Self {
        self.remap = Some(remap);
        self
    }
    const fn keywords(mut self, keywords: &'static str) -> Self {
        self.keywords = keywords;
        self
    }
    const fn cautious(mut self) -> Self {
        self.danger = DangerLevel::Cautious;
        self
    }
    const fn dangerous(mut self) -> Self {
        self.danger = DangerLevel::Dangerous;
        self
    }
}

const F4: Key = Key {
    modifiers: crate::shell::keymap::KeyModifiers::NONE,
    code: KeyCode::F(4),
};

// Scenes

/// Verbs owned by the live thread scene alone.
const THREAD: &[Verb] = &[
    verb(
        "thread.switch-plane",
        "Switch focus: composer ⇄ transcript",
        "Move the keyboard between the draft and the conversation",
        Key::BACKTAB,
        InputAction::ToggleFocusPlane,
    )
    .keywords("plane transcript composer"),
    verb(
        "thread.search",
        "Search transcript",
        "Find text anywhere in this thread",
        Key::ctrl('f'),
        InputAction::OpenTranscriptSearch,
    )
    .keywords("find grep"),
    verb(
        "thread.recall",
        "Recall a previous prompt",
        "Search everything you have sent and insert it into the composer",
        Key::CTRL_R,
        InputAction::OpenHistory,
    )
    .remap(SurfaceVerb::OpenHistory)
    .requires(Requires::ComposerPlane)
    .keywords("history"),
    verb(
        "thread.send-mode",
        "Toggle steer / follow-up",
        "Choose whether Enter steers the running round or queues a follow-up",
        Key::TAB,
        InputAction::ToggleSendMode,
    )
    .remap(SurfaceVerb::ToggleSendMode)
    .requires(Requires::Running)
    .keywords("queue steer"),
    verb(
        "thread.interrupt",
        "Interrupt the running round",
        "Stop the agent's current turn; the thread is kept",
        Key::ESC,
        InputAction::Interrupt,
    )
    .requires(Requires::Running)
    .cautious()
    .keywords("stop cancel abort"),
];

/// Verbs of every chat-shaped scene (thread and subagent).
const CHAT: &[Verb] = &[
    verb(
        "chat.prev-entry",
        "Highlight previous entry",
        "Walk up through tool steps, notices and reasoning",
        Key::CTRL_P,
        InputAction::FocusPrevTarget,
    )
    .remap(SurfaceVerb::FocusPrevTarget),
    verb(
        "chat.next-entry",
        "Highlight next entry",
        "Walk down through tool steps, notices and reasoning",
        Key::CTRL_N,
        InputAction::FocusNextTarget,
    )
    .remap(SurfaceVerb::FocusNextTarget),
    verb(
        "chat.expand",
        "Expand or collapse entry",
        "Disclose the highlighted entry's body in place",
        Key::plain(' '),
        InputAction::ActivateFocusedTarget,
    )
    .requires(Requires::FocusedEntry)
    .keywords("open fold toggle"),
    verb(
        "chat.view-log",
        "View full log",
        "Open the highlighted command's complete output as a page",
        Key::plain('v'),
        InputAction::ViewFocusedTargetOutput,
    )
    .requires(Requires::FocusedEntry)
    .keywords("output"),
    verb(
        "chat.copy-entry",
        "Copy entry",
        "Copy the highlighted entry to the clipboard",
        Key::plain('y'),
        InputAction::CopyFocusedTarget,
    )
    .requires(Requires::FocusedEntry)
    .keywords("yank clipboard"),
    verb(
        "chat.copy-diagnostic",
        "Copy entry as diagnostic",
        "Copy with kind, severity, route, attempt, timestamp and log path",
        Key::plain('Y'),
        InputAction::CopyFocusedDiagnostic,
    )
    .requires(Requires::FocusedEntry)
    .keywords("bug report yank"),
    verb(
        "chat.top",
        "Jump to start",
        "Scroll the transcript to its first entry",
        Key::HOME,
        InputAction::ScrollTop,
    )
    .remap(SurfaceVerb::ScrollTop)
    .requires(Requires::TranscriptPlane)
    .keywords("top beginning"),
    verb(
        "chat.bottom",
        "Jump to latest",
        "Scroll to the newest entry and follow new output",
        Key::END,
        InputAction::ScrollBottom,
    )
    .remap(SurfaceVerb::ScrollBottom)
    .requires(Requires::TranscriptPlane)
    .keywords("bottom end follow"),
    verb(
        "chat.diff",
        "Review file changes",
        "Open the diff viewer for every file this session changed",
        F4,
        InputAction::OpenDiffViewer,
    )
    .keywords("diff patch changes"),
];

const SUBAGENT: &[Verb] = &[verb(
    "subagent.back",
    "Back to parent",
    "Leave this subagent and return to the thread that spawned it",
    Key::ALT_BRACKET_LEFT,
    InputAction::TabHistoryBack,
)
.remap(SurfaceVerb::SceneHistoryBack)
.keywords("parent return")];

const COMMAND_LOG: &[Verb] = &[
    verb(
        "log.copy",
        "Copy log",
        "Copy the log in its current scope",
        Key::plain('y'),
        InputAction::CommandLogCopy,
    )
    .keywords("yank clipboard"),
    verb(
        "log.scope",
        "Toggle log scope",
        "Switch between the hidden rows and the whole output",
        Key::plain('h'),
        InputAction::CommandLogToggleScope,
    ),
    verb(
        "log.top",
        "Jump to start",
        "Scroll to the first line",
        Key::HOME,
        InputAction::ScrollTop,
    ),
    verb(
        "log.bottom",
        "Jump to end",
        "Scroll to the last line",
        Key::END,
        InputAction::ScrollBottom,
    ),
    verb(
        "log.back",
        "Back to transcript",
        "Close the log and return to where you were",
        Key::ESC,
        InputAction::SceneBack,
    )
    .keywords("close leave"),
];

const DIFF: &[Verb] = &[
    verb(
        "diff.next-file",
        "Next file",
        "Jump to the next changed file",
        Key::plain('n'),
        InputAction::DiffNextFile,
    ),
    verb(
        "diff.prev-file",
        "Previous file",
        "Jump to the previous changed file",
        Key::plain('p'),
        InputAction::DiffPrevFile,
    ),
    verb(
        "diff.reviewed",
        "Mark file reviewed",
        "Toggle the reviewed mark on the current file",
        Key::plain('r'),
        InputAction::DiffToggleReviewed,
    )
    .keywords("done check"),
    verb(
        "diff.top",
        "Jump to start",
        "Scroll to the top of the diff",
        Key::HOME,
        InputAction::ScrollTop,
    ),
    verb(
        "diff.bottom",
        "Jump to end",
        "Scroll to the bottom of the diff",
        Key::END,
        InputAction::ScrollBottom,
    ),
    verb(
        "diff.back",
        "Back to transcript",
        "Close the diff viewer",
        Key::ESC,
        InputAction::SceneBack,
    )
    .keywords("close leave"),
];

const DASHBOARD: &[Verb] = &[
    verb(
        "dashboard.preview",
        "Preview session",
        "Open a read-only preview of the selected session",
        Key::ENTER,
        InputAction::HostPreviewSelected,
    ),
    verb(
        "dashboard.attach",
        "Attach to session",
        "Switch this client to the selected session",
        Key::plain('a'),
        InputAction::HostSwitchSelected,
    )
    .keywords("open switch"),
    verb(
        "dashboard.prompt",
        "Send a task",
        "Type a prompt for the selected session",
        Key::plain('p'),
        InputAction::HostPromptOpen,
    )
    .keywords("message"),
    verb(
        "dashboard.new",
        "New session",
        "Create a session, optionally with an opening task",
        Key::plain('n'),
        InputAction::HostNewSession,
    )
    .keywords("create spawn"),
    verb(
        "dashboard.filter",
        "Filter sessions",
        "Narrow the fleet list by text",
        Key::plain('/'),
        InputAction::HostFilterOpen,
    )
    .keywords("search find"),
    verb(
        "dashboard.usage",
        "Toggle usage view",
        "Switch between the session fleet and model usage analytics",
        Key::plain('u'),
        InputAction::HostToggleUsage,
    )
    .keywords("analytics tokens cost"),
    verb(
        "dashboard.pane",
        "Switch pane",
        "Move focus between the list and the detail pane",
        Key::TAB,
        InputAction::HostFocusToggle,
    ),
    verb(
        "dashboard.interrupt",
        "Interrupt session",
        "Stop the selected session's current round",
        Key::plain('i'),
        InputAction::HostInterruptSelected,
    )
    .cautious()
    .keywords("stop"),
    verb(
        "dashboard.suspend",
        "Suspend session",
        "Park the selected session; it resumes on the next attach",
        Key::plain('s'),
        InputAction::HostSuspendSelected,
    )
    .cautious()
    .keywords("park pause"),
    verb(
        "dashboard.kill",
        "Kill session",
        "Tear the selected session down; press k again to confirm",
        Key::plain('k'),
        InputAction::HostKillSelected,
    )
    .dangerous()
    .keywords("terminate end"),
];

const SETTINGS: &[Verb] = &[
    verb(
        "settings.activate",
        "Change setting",
        "Activate or edit the highlighted setting",
        Key::ENTER,
        InputAction::ConfigActivate,
    )
    .keywords("edit toggle"),
    verb(
        "settings.prev-segment",
        "Previous section tab",
        "Switch to the previous tab of this settings page",
        Key::LEFT,
        InputAction::ConfigSegmentPrev,
    )
    .requires(Requires::SettingsDetail),
    verb(
        "settings.next-segment",
        "Next section tab",
        "Switch to the next tab of this settings page",
        Key::RIGHT,
        InputAction::ConfigSegmentNext,
    )
    .requires(Requires::SettingsDetail),
];

/// The verb groups of a scene, most specific first.
pub(crate) fn scene_verbs(scene: SceneKind) -> &'static [&'static [Verb]] {
    match scene {
        SceneKind::Thread => &[THREAD, CHAT],
        SceneKind::Subagent => &[SUBAGENT, CHAT],
        SceneKind::CommandLog => &[COMMAND_LOG],
        SceneKind::Diff => &[DIFF],
        SceneKind::Dashboard => &[DASHBOARD],
        SceneKind::Settings => &[SETTINGS],
    }
}

// Dialogs

const THREADS: &[Verb] = &[
    verb(
        "threads.open",
        "Open thread",
        "Attach to the highlighted thread",
        Key::ENTER,
        InputAction::OpenSelectedSession,
    ),
    verb(
        "threads.new",
        "New thread",
        "Start a fresh thread",
        Key::plain('n'),
        InputAction::CreateNewSession,
    ),
    verb(
        "threads.info",
        "Thread info",
        "Show the highlighted thread's details",
        Key::plain('i'),
        InputAction::OpenSessionInfo,
    ),
    verb(
        "threads.forks",
        "Expand or collapse forks",
        "Show the highlighted thread's branches",
        Key::TAB,
        InputAction::ToggleSessionTimelineExpand,
    )
    .keywords("branches tree"),
    verb(
        "threads.delete",
        "Delete thread",
        "Permanently delete the highlighted thread",
        Key::plain('d'),
        InputAction::DeleteSelectedSession,
    )
    .dangerous()
    .keywords("remove"),
];

const MODELS: &[Verb] = &[
    verb(
        "models.select",
        "Use model",
        "Switch this session to the highlighted model",
        Key::ENTER,
        InputAction::ProviderPickerActivate,
    )
    .keywords("select switch"),
    verb(
        "models.search",
        "Search models",
        "Filter the list by name",
        Key::plain('/'),
        InputAction::ModelEnterSearch,
    )
    .keywords("find filter"),
    verb(
        "models.favorite",
        "Toggle favorite",
        "Star the highlighted model so it sorts first",
        Key::plain('*'),
        InputAction::ProviderPickerToggleFavorite,
    )
    .keywords("star pin"),
    verb(
        "models.details",
        "Model details",
        "Inspect limits, pricing and settings",
        Key::plain('i'),
        InputAction::OpenModelDetail,
    )
    .keywords("info inspect"),
    verb(
        "models.edit",
        "Edit model settings",
        "Tune effort, thinking and capability overrides",
        Key::plain('e'),
        InputAction::OpenModelEditor,
    ),
    verb(
        "models.refresh",
        "Refresh models",
        "Fetch the provider's current model list",
        Key::plain('r'),
        InputAction::RefreshProviderModels,
    )
    .keywords("reload"),
    verb(
        "models.block",
        "Block model",
        "Hide the highlighted model from routing",
        Key::plain('x'),
        InputAction::ProviderPickerBlockModel,
    )
    .cautious()
    .keywords("hide disable"),
];

const CONNECTIONS: &[Verb] = &[
    verb(
        "connections.details",
        "Connection details",
        "Inspect the highlighted connection",
        Key::ENTER,
        InputAction::OpenConnectionDetail,
    )
    .keywords("info"),
    verb(
        "connections.search",
        "Search connections",
        "Filter the list by name",
        Key::plain('/'),
        InputAction::ModelEnterSearch,
    )
    .keywords("find filter"),
    verb(
        "connections.add-preset",
        "Add provider",
        "Connect a provider from the curated presets",
        Key::plain('a'),
        InputAction::OpenPresetChooser,
    )
    .keywords("new connect login"),
    verb(
        "connections.add-custom",
        "Add custom provider",
        "Connect any OpenAI- or Anthropic-compatible endpoint",
        Key::plain('c'),
        InputAction::OpenCustomConnection,
    )
    .keywords("new endpoint"),
    verb(
        "connections.edit",
        "Edit provider",
        "Change the highlighted provider's settings",
        Key::plain('e'),
        InputAction::OpenModelEditor,
    ),
    verb(
        "connections.refresh",
        "Refresh provider models",
        "Fetch the highlighted provider's model list",
        Key::plain('r'),
        InputAction::RefreshProviderModels,
    )
    .keywords("reload"),
    verb(
        "connections.delete",
        "Delete provider",
        "Remove the highlighted custom provider (asks first)",
        Key::plain('D'),
        InputAction::DeleteProvider,
    )
    .dangerous()
    .keywords("remove"),
];

const MCP: &[Verb] = &[
    verb(
        "mcp.toggle",
        "Enable or disable server",
        "Toggle the highlighted MCP server",
        Key::plain(' '),
        InputAction::McpToggle,
    ),
    verb(
        "mcp.reconnect",
        "Reconnect server",
        "Restart the highlighted MCP server's connection",
        Key::plain('r'),
        InputAction::McpReconnect,
    )
    .keywords("restart retry"),
];

const PERMISSIONS: &[Verb] = &[
    verb(
        "permissions.toggle",
        "Revoke or restore rule",
        "Toggle the highlighted always-allow rule",
        Key::plain(' '),
        InputAction::PermissionsActivate,
    ),
    verb(
        "permissions.clear",
        "Revoke all rules",
        "Drop every always-allow rule for this session",
        Key::plain('c'),
        InputAction::PermissionsClearAll,
    )
    .dangerous()
    .keywords("reset clear"),
];

const QUEUE: &[Verb] = &[
    verb(
        "queue.recall",
        "Recall into composer",
        "Take the highlighted message back for editing",
        Key::ENTER,
        InputAction::RecallQueuedSelected,
    )
    .keywords("edit"),
    verb(
        "queue.up",
        "Move up",
        "Send the highlighted message sooner",
        Key::plain('K'),
        InputAction::QueueMoveItem { delta: -1 },
    )
    .keywords("reorder earlier"),
    verb(
        "queue.down",
        "Move down",
        "Send the highlighted message later",
        Key::plain('J'),
        InputAction::QueueMoveItem { delta: 1 },
    )
    .keywords("reorder later"),
    verb(
        "queue.delete",
        "Delete queued message",
        "Drop the highlighted message without sending it",
        Key::plain('D'),
        InputAction::QueueDelete,
    )
    .dangerous()
    .keywords("remove"),
];

const THREAD_STATS: &[Verb] = &[verb(
    "stats.trace",
    "Open session trace",
    "Drill from totals into per-round attempts",
    Key::plain('t'),
    InputAction::ThreadStatsOpenTrace,
)];

const SESSION_TRACE: &[Verb] = &[verb(
    "trace.inspect",
    "Inspect round",
    "Show the highlighted round's attempts in detail",
    Key::ENTER,
    InputAction::TraceInspectDetail,
)];

/// The context-plan inspector has exactly one verb of its own: the block body
/// disclosure. Everything else it shows is a measurement, not an action.
const CONTEXT_PLAN: &[Verb] = &[verb(
    "context.body",
    "Show or hide block body",
    "Expand the selected block's rendered text in place",
    Key::ENTER,
    InputAction::ContextPlanToggleBody,
)
.keywords("source prompt expand inspect")];

const SKILLS: &[Verb] = &[verb(
    "skills.details",
    "Toggle skill details",
    "Show or hide the highlighted skill's description",
    Key::ENTER,
    InputAction::SkillsToggleDetail,
)];

const TOOLS: &[Verb] = &[verb(
    "tools.details",
    "Tool details",
    "Show the highlighted tool's schema and contract",
    Key::ENTER,
    InputAction::OpenToolDetail,
)];

const QUOTAS: &[Verb] = &[
    verb(
        "quotas.details",
        "Toggle quota details",
        "Show the highlighted provider's limits",
        Key::ENTER,
        InputAction::QuotasToggleDetail,
    ),
    verb(
        "quotas.refresh",
        "Refresh quotas",
        "Query every provider's remaining quota",
        Key::plain('r'),
        InputAction::RefreshQuotas,
    )
    .keywords("reload"),
];

const HISTORY: &[Verb] = &[
    verb(
        "history.insert",
        "Insert prompt",
        "Put the highlighted prompt into the composer",
        Key::ENTER,
        InputAction::HistoryInsert,
    ),
    verb(
        "history.delete",
        "Delete prompt",
        "Permanently remove the highlighted prompt from history",
        Key::SHIFT_DELETE,
        InputAction::HistoryDeleteSelected,
    )
    .dangerous(),
];

const PORT_SHARE: &[Verb] = &[
    verb(
        "port.toggle",
        "Start or stop listener",
        "Toggle the TCP listener remote clients attach to",
        Key::plain(' '),
        InputAction::TogglePortListener,
    )
    .cautious()
    .keywords("share remote network"),
    verb(
        "port.copy",
        "Copy connect command",
        "Copy the nuo --remote command for another machine",
        Key::plain('y'),
        InputAction::CopyPortConnectInfo,
    )
    .keywords("clipboard"),
];

const NOTICES: &[Verb] = &[
    verb(
        "notices.details",
        "Toggle notice details",
        "Expand the highlighted notice",
        Key::ENTER,
        InputAction::NoticesToggleDetail,
    ),
    verb(
        "notices.copy",
        "Copy notice",
        "Copy the highlighted notice as a diagnostic",
        Key::plain('y'),
        InputAction::NoticesCopy,
    )
    .keywords("clipboard"),
    verb(
        "notices.clear",
        "Clear notices",
        "Empty the notice log",
        Key::plain('x'),
        InputAction::NoticesClear,
    )
    .cautious(),
];

/// The verbs of a dialog. The palette is never opened over itself, Help is a
/// read-only reference (`[INV-PLANE-02]`), and the read-only reports have
/// nothing to run.
pub(crate) fn dialog_verbs(dialog: DialogKind) -> &'static [Verb] {
    match dialog {
        DialogKind::Threads => THREADS,
        DialogKind::Models => MODELS,
        DialogKind::Connections => CONNECTIONS,
        DialogKind::Mcp => MCP,
        DialogKind::Permissions => PERMISSIONS,
        DialogKind::Queue => QUEUE,
        DialogKind::ThreadStats => THREAD_STATS,
        DialogKind::SessionTrace => SESSION_TRACE,
        DialogKind::ContextPlan => CONTEXT_PLAN,
        DialogKind::Skills => SKILLS,
        DialogKind::Tools => TOOLS,
        DialogKind::Quotas => QUOTAS,
        DialogKind::HistorySearch => HISTORY,
        DialogKind::PortShare => PORT_SHARE,
        DialogKind::Notices => NOTICES,
        DialogKind::UsageStats | DialogKind::SessionTree | DialogKind::Help => &[],
        DialogKind::Palette => &[],
    }
}
