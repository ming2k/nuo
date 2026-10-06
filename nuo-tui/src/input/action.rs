//! The semantic action vocabulary the dispatcher produces.

/// Which OAuth pending-sheet field to copy: the device verification code (the
/// value the user pastes at github.com/login/device) or the verification URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OauthCopyTarget {
    UserCode,
    Url,
    Selected,
}

/// Result of processing an input event.
#[derive(Debug, PartialEq)]
pub enum InputAction {
    /// Nothing to do.
    None,
    /// Quit the application.
    Quit,
    /// Send a chat message.
    SendChat(String),
    /// Immediate steering intervention (while running).
    SteerImmediate(String),
    /// Enqueue follow-up prompt into outbox queue (while running).
    QueueFollowUp(String),
    /// Toggle between steer and follow-up queue mode while running.
    ToggleSendMode,
    /// Send a slash command.
    SendSlash(String),
    /// Activate the highlighted row of the **Models** picker: a flat
    /// (provider, model) pair. Falls through to the API-key setup modal when
    /// the target has no key. The Connections list has no activate concept —
    /// it only manages instances (`a`/`e`/`D`), leaving provider switching to
    /// this picker.
    ProviderPickerActivate,
    /// Drill into the connection detail sub-view for the highlighted connection row in Connections modal.
    OpenConnectionDetail,
    /// Open the connection detail modal directly for the active connection (via click or Ctrl+N).
    OpenActiveConnectionDetail,
    /// Toggle expansion of the served models list in the connection detail modal.
    ToggleConnectionModelsExpanded,
    /// Toggle the favorite flag on the highlighted Models row (model-level,
    /// ADR-0046). The Connections list has no favorite concept.
    ProviderPickerToggleFavorite,
    /// Block/intercept the highlighted model from the connection pipe (`x` in Models modal, ADR-0203 §10).
    ProviderPickerBlockModel,
    /// Open the unified provider editor (`e`): the per-model settings editor
    /// for the highlighted Models row, or the provider editor (key / meta) for
    /// the highlighted Connections row.
    OpenModelEditor,
    /// Submit the unified provider editor: persist the entered key / model-id and
    /// activate the target model.
    SubmitModelEditor,
    /// Cycle focus between the editor's fields (API key ↔ effort).
    ModelEditorNextField,
    /// Cycle the effort selector (←/→) on the Anthropic key editor's effort
    /// field. Carries a delta of ±1; wraps around the effort levels.
    ModelEditorEffortCycle {
        delta: i8,
    },
    /// Jump the effort selector straight to a tier (digit `1`..=`7` on the
    /// ladder, `0`-indexed here) on the effort field — the flat segmented
    /// layout makes direct selection the natural counterpart to ←/→ stepping.
    /// Ignored when the index is past the model's ladder.
    ModelEditorEffortJump {
        index: usize,
    },
    /// Toggle extended thinking on/off (Space) on the Anthropic key editor's
    /// thinking field. Orthogonal to effort.
    ModelEditorThinkingToggle,
    /// Cycle the vision capability override (ADR-0149 layer 1) tri-state:
    /// inherit → force on → force off. Field 3 of the settings editor.
    ModelEditorVisionCycle,
    /// Cycle the tool-call capability override tri-state. Field 4.
    ModelEditorToolCycle,
    /// Submit the custom-provider editor → `AgentRequest::AddConnection`.
    SubmitCustomProvider,
    /// Cancel the custom-provider editor and return to the Connections list.
    CancelCustomProvider,
    /// Move focus to the next / previous field of the custom-provider editor
    /// (`Tab` / `BackTab`), wrapping at the ends.
    CustomProviderNextField,
    CustomProviderPrevField,
    /// Scroll the custom-provider form with `↑` / `↓`. `forward` = down.
    ScrollCustomProvider {
        forward: bool,
    },
    /// Cycle the focused custom provider selector (`Protocol` or
    /// `ClientIdentity`) with `←` / `→`.
    CycleCustomProviderChoice {
        forward: bool,
    },
    /// Move the preset-chooser selection with `↑` / `↓`. `forward` = down.
    MovePresetChoice {
        forward: bool,
    },
    /// Open the provider editor seeded from the highlighted preset (`Enter`).
    SelectPreset,
    /// Select the highlighted OAuth preset with an explicit login method.
    /// The preset chooser exposes `b` for browser PKCE and `d` for device
    /// authorization when the client registration supports them.
    SelectPresetWithOauthMethod {
        method: nuo_wire::LoginMethod,
    },
    /// Cancel the preset chooser and return to the Connections list.
    CancelPresetChooser,
    /// Cancel the "Add preset connection → OAuth" browser flow (`Esc` while
    /// `Modal::OauthPending` is active).
    CancelOauthPending,
    /// Cycle focus between copyable targets (URL and device code) in OAuth pending sheet (`Tab`/`Left`/`Right`).
    CycleOauthSelection,
    /// Copy the OAuth pending sheet's primary content. `user_code` copies the
    /// device-verification code the user must paste at github.com/login/device;
    /// `url` copies the verification URL; `Selected` copies the focused card.
    CopyOauthContent {
        target: OauthCopyTarget,
    },
    /// Delete the entire highlighted custom provider from the Connections list
    /// (`Shift+D`). Built-in providers are ignored by the handler. Opens the
    /// provider-delete confirm overlay rather than deleting immediately.
    DeleteProvider,
    /// Confirm the pending provider-delete: dispatch the staged
    /// `AgentRequest::DeleteConnection` and close the confirm overlay. Only
    /// produced by the confirm overlay's Enter when focus is on Delete.
    DeleteProviderConfirm,
    /// Cancel the provider-delete confirm overlay: drop the staged provider id
    /// and return focus to the Connections list. Produced by Esc / Ctrl+C
    /// / Enter-on-Cancel inside the confirm overlay.
    DeleteProviderCancel,
    /// Interrupt current operation.
    Interrupt,
    /// Open the flat Models picker (`/models`, Ctrl+M) — the daily-driver
    /// model-switch surface.
    OpenSessions,
    OpenModels,
    /// Open the Connections list (`/connections`) — the provider-instance
    /// management surface.
    OpenConnections,
    /// Refresh available models for catalog-enabled providers from upstream.
    RefreshProviderModels,
    /// Open the curated preset chooser (`a` in the Connections modal).
    OpenPresetChooser,
    /// Open the standalone custom-connection editor (`c` in Connections).
    OpenCustomConnection,
    /// Open the input-history modal (Ctrl+R). Opens in browse mode — a plain
    /// newest-first list; `/` then enters the search sub-layer.
    OpenHistory,
    /// Open the queue overview modal (the full outbox list). Reached via `F2`
    /// or by clicking the persistent queue bar. Mirrors clicking the queue bar
    /// — the request is never forwarded, it only opens the overlay.
    OpenQueue,
    /// Open the permissions manager modal: a centered list of cached "always
    /// allow" rules with per-row revoke and clear-all. Reached via the
    /// `/permissions` slash command (intercepted locally, never sent to the
    /// backend). `/permissions clear` still goes to the backend.
    OpenPermissions,
    /// Open the tools manager modal: a centered, selectable list of every
    /// session tool with a `Space` toggle. Reached via the `/tools` slash
    /// command (intercepted locally, never sent to the backend). The request is
    /// never forwarded — it only opens the overlay.
    OpenTools,
    /// Open the usage-statistics overlay (`/usage`, ADR-0122): daily token
    /// totals, per-model breakdown, and the recent request event log, from
    /// the durable cross-session store. Intercepted locally; the handler
    /// issues `AgentRequest::QueryUsageStats` on every open, so the overlay
    /// always reflects the daemon-side store (the report is fetched on demand,
    /// never pushed — see ADR-0209's 2026-09-11 addendum). While the reply is
    /// in flight the previously rendered numbers stay on screen.
    OpenUsage,
    /// Open the MCP manager modal: a centered, selectable list of every
    /// configured MCP server with `Space` toggle and `r` reconnect. Reached via
    /// the `/mcp` slash command (intercepted locally, never sent to the
    /// backend). The request is never forwarded — it only opens the overlay.
    OpenMcp,
    /// Open the skills modal: a centered, selectable list of every loaded
    /// skill with a per-row detail expansion. Reached via
    /// the `/skills` slash command (intercepted locally, never sent to the
    /// backend; `/skills list` with args still forward).
    /// The request is never forwarded — it only opens the overlay.
    OpenSkills,
    /// Toggle the detail expansion of the selected skill row in the skills
    /// modal. Bound to `Enter`.
    SkillsToggleDetail,
    /// Open the config manager modal: a centered list of configurable
    /// categories (Appearance and Layout). Reached via the `/config` slash command
    /// (intercepted locally, never sent to the backend). `Enter` / `Space`
    /// on a category drills into its sub-page.
    OpenConfig,
    /// Connect/disconnect the selected MCP server in the MCP manager modal.
    /// Bound to `Space`.
    McpToggle,
    /// Reconnect the selected MCP server in the MCP manager modal. Bound to `r`.
    McpReconnect,
    /// Revoke the selected "always allow" rule in the permissions manager
    /// modal. Bound to `Space`.
    PermissionsActivate,
    /// Clear every cached "always allow" rule. Bound to `c` in the
    /// permissions manager modal.
    PermissionsClearAll,
    /// Activate or toggle the selected item in the Settings View. Bound to `Enter` / `Space`.
    ConfigActivate,
    /// Delete the selected custom connection instance in Settings View. Bound to `d` / `D`.
    /// Switch to previous tab segment in Settings View (e.g. Web Search tab). Bound to `←` / `1` / `h`.
    ConfigSegmentPrev,
    /// Switch to next tab segment in Settings View (e.g. Web Fetch tab). Bound to `→` / `2` / `l`.
    ConfigSegmentNext,
    /// Toggle focus between Categories and Detail in the Settings View. Bound to `Tab`.
    ConfigFocusToggle,
    /// Move the tool-selection cursor in the session-context dashboard when it
    /// still hosts the tools list, and in the tools manager modal otherwise.
    /// `forward` = down, else up.
    SessionSelect {
        forward: bool,
    },
    /// Toggle the selected tool's enabled flag in the tools manager modal.
    /// Bound to `Space`.
    SessionActivate,
    /// Open the currently-selected session in the sessions picker.
    OpenSelectedSession,
    /// Toggle expand/fold of child timeline branches (asides/forks) in the sessions picker (ADR-0251).
    ToggleSessionTimelineExpand,
    /// `/host` panel Enter: switch the TUI to drive the selected daemon
    /// session (ADR-0096). Handled by exiting to re-attach.
    HostSwitchSelected,
    /// Dashboard Enter on a dock selection: open the read-only preview modal
    /// for that session (ADR-0097 §3). Selection alone never triggers this —
    /// only an explicit Enter.
    HostPreviewSelected,
    /// Dashboard `Tab`: toggle keyboard focus between the session list and
    /// the detail pane.
    HostFocusToggle,
    /// Dashboard `i`: interrupt the selected session's current round
    /// (control-plane verb, ADR-0096).
    HostInterruptSelected,
    /// Dashboard `k`: kill (tear down) the selected session — a two-press
    /// confirm, since a session's running work dies with it. The receipt
    /// lands in the console log.
    HostKillSelected,
    /// Dashboard `s`: suspend the selected session (park it in memory; the
    /// next attach rebuilds it via lazy resume). Refused while a client is
    /// attached or a round is active.
    HostSuspendSelected,
    /// Dashboard `p`: open the inline prompt to send a task to the selected
    /// session. While open, Enter submits the prompt text.
    HostPromptOpen,
    /// Dashboard `n`: open the inline new-session prompt (create + optional
    /// opening task). While open, Enter creates the session.
    HostNewSession,
    /// Dashboard printable key with no prompt open: open the inline prompt
    /// seeded with the typed char — the console is a command surface, so
    /// typing `@3 …` starts the composer directly instead of requiring a
    /// `p` first.
    HostPromptSeed(char),
    /// Dashboard inline-prompt submit (Enter while `p`/`n` is open).
    HostPromptSubmit,
    /// Drill into the selected round or turn in the Telemetry modal. Bound to `Enter`.
    TelemetryActivate,
    /// Advance to the next tab in the Telemetry modal (Tab / Right).
    TelemetryNextTab,
    /// Return to the previous tab in the Telemetry modal (Shift+Tab / Left).
    TelemetryPrevTab,
    /// Switch directly to a specific tab in the Telemetry modal ('1' / '2').
    TelemetrySetTab(crate::TelemetryTab),
    /// Delete the currently-selected session in the sessions picker.
    DeleteSelectedSession,
    /// Create a brand new session from the sessions picker ('n' / 'N').
    CreateNewSession,
    /// Open the session-info sub-view for the selected session ('i'). Shows the
    /// full last effective prompt, creation time, and message count.
    OpenSessionInfo,
    /// Close any modal.
    CloseModal,
    /// Scroll up.
    ScrollUp,
    /// Scroll down.
    ScrollDown,
    /// Mouse wheel tick at a screen position. Semantically the same intent as
    /// [`InputAction::ScrollUp`]/[`InputAction::ScrollDown`], but carrying the
    /// pointer cell so the handler can route spatially: a tick landing inside
    /// the composer panel scrolls the input's own viewport instead of the
    /// transcript (the panel is a scroll region when the draft outgrows the
    /// box). Keyboard-driven scroll keeps the bare variants.
    Wheel {
        up: bool,
        x: u16,
        y: u16,
    },
    /// Scroll up by one viewport page.
    ScrollPageUp,
    /// Scroll down by one viewport page.
    ScrollPageDown,
    /// Scroll to the very top.
    ScrollTop,
    /// Scroll to the very bottom and re-engage auto-follow.
    ScrollBottom,
    /// Copy current selection.
    CopySelection,
    /// Plain Ctrl+C: copy selection, clear input, or arm quit. It never
    /// interrupts a running turn — only double-Esc does.
    CtrlC,
    /// Open the unified session telemetry report — the drill-down behind the model
    /// bar's context meter and rate gauge. Keyboard twin of clicking those gauges (`Ctrl+O`).
    OpenTelemetry,
    /// Move keyboard focus to the next activatable target. When no target is
    /// focused yet, focuses the first (oldest) step. Driven by `Ctrl+N` and by
    /// `↓` while a step is already focused.
    FocusNextTarget,
    /// Move keyboard focus to the previous activatable target. When no target
    /// is focused yet, focuses the last (nearest-to-prompt) step. Driven by
    /// `Ctrl+P` and by `↑` while a step is already focused.
    FocusPrevTarget,
    /// Activate the current keyboard-focused target (`Enter`).
    ActivateFocusedTarget,
    /// Send a key event to the focused transcript target at stack top.
    FocusedTargetKey(crate::keymap::Key),
    /// Copy the content of the currently focused target (`y` or `c` while a step is focused).
    CopyFocusedTarget,
    /// Clear the keyboard-focused target, returning every key to its ordinary
    /// input-box meaning. Triggered by `Esc` while a step is focused.
    ClearFocusedTarget,
    /// Cancel the inline ↑/↓ history recall and restore the stashed draft
    /// (text + attachments). Triggered by `Esc` while the recall pointer sits
    /// on a history row — the universal "get me back" chord must exit the
    /// recall state, which otherwise has no exit short of walking ↓ to the
    /// end or sending (ADR-0192).
    CancelHistoryRecall,
    /// Paste from the system clipboard (image or text). Resolved by the app
    /// loop, which reads the clipboard asynchronously.
    Paste,
    /// Terminal-level bracketed paste. The text payload is already available;
    /// the app loop routes it through the same chip-or-inline logic as
    /// [`InputAction::Paste`].
    BracketedPaste(String),
    /// Input character.
    InsertChar(char),
    /// Delete character before cursor.
    Backspace,
    /// Delete character after the cursor (the `Del` key's forward delete).
    /// The input layer has already spliced the text; the action signals the
    /// event loop to run the same post-edit passes as `Backspace`
    /// (completion latch reset, focus reclaim, attachment reconcile).
    DeleteForward,
    /// Cycle suggestion forward.
    SuggestNext,
    /// Cycle suggestion backward.
    SuggestPrev,
    /// Accept the next/previous completion item by index without closing the
    /// popup. Used by `Tab`, which cycles through candidates one splice at a
    /// time. The popup re-renders against the spliced input so the user can
    /// keep cycling.
    AcceptSuggestion(String),
    /// Re-open a completion menu that Esc dismissed, without accepting
    /// anything. Bound to `Tab` while the composer still holds the trigger
    /// text (a partial `/command` or an `@mention`): Esc closes the popup,
    /// Tab brings it back, so the toggle is symmetric and the user never has
    /// to re-edit the text to recover the menu.
    ReopenCompletion,
    /// Like [`InputAction::AcceptSuggestion`] but the popup is closed
    /// afterwards. Used by `Enter` (both the slash-prefix auto-accept and the
    /// highlighted-item path). The harness latches a `completion_dismissed`
    /// flag so the popup stays hidden until the next `InsertChar` /
    /// `Backspace`, matching the expectation that pressing Enter "finishes"
    /// the current completion.
    CommitSuggestion(String),
    /// Dismiss the completion popup without accepting anything. Used by `Esc`
    /// when a slash/path completion menu is open. Latches the same
    /// `completion_dismissed` flag as [`InputAction::CommitSuggestion`] so the
    /// popup stays hidden until the next edit clears the latch.
    CloseCompletion,
    /// Navigate history up.
    HistoryPrev,
    /// Navigate history down.
    HistoryNext,
    /// Re-edit the queue modal's *selected* item (not always the newest):
    /// recall it into the composer and close the modal. Bound to `Enter`
    /// inside the queue modal. The queue is auto-blocked on modal open, so
    /// this is always safe.
    RecallQueuedSelected,
    /// Toggle the user block on the viewed session's outbox. While blocked,
    /// no queued message auto-drains — not even after the round completes.
    /// Reachable from `Ctrl+P` (bar, no modal) and the queue modal's block
    /// control.
    QueueToggleBlock,
    /// Delete the queue modal's selected item. Bound to `D` inside the queue
    /// modal (matching the destructive-delete convention in Connections /
    /// Sessions).
    QueueDelete,
    /// Move the queue modal's selected item one slot. `delta = -1` toward the
    /// front (next to pop), `delta = 1` toward the tail. Bound to `K` / `J`
    /// (vim convention) inside the queue modal.
    QueueMoveItem {
        delta: i32,
    },
    /// Accept the focused entry in the Ctrl+R history modal (Enter, in either
    /// browse or search mode): insert it into the input box and close the modal.
    /// The message is not sent — the user can edit and press Enter again to ship
    /// it.
    HistoryInsert,
    /// Delete the focused entry in the Ctrl+R history modal (Shift+Delete):
    /// permanently remove it from in-memory history, drop from SQLite, and
    /// clamp modal selection.
    HistoryDeleteSelected,
    /// Enter the model picker's search sub-layer (`/` in browse mode): start
    /// borrowing the composer line as a live fuzzy query and re-rank the list.
    ModelEnterSearch,
    /// Leave the model picker's search sub-layer (first Esc while searching):
    /// clear the query and return to the full browse list. A second Esc then
    /// closes the modal.
    ModelExitSearch,
    /// Select modal item up.
    ModalUp,
    /// Select modal item down.
    ModalDown,
    /// Submit the selected permission decision.
    PermissionSubmit,
    /// Reject the active permission request.
    PermissionReject,
    /// Return from the always-allow confirmation step.
    PermissionBack,
    /// Scroll the expanded "Details" body of the permission sheet up a row.
    PermissionDetailsUp,
    /// Scroll the expanded "Details" body of the permission sheet down a row.
    PermissionDetailsDown,
    /// Move the selection up inside the question modal.
    QuestionUp,
    /// Move the selection down inside the question modal.
    QuestionDown,
    /// Toggle/select the currently highlighted question option. For
    /// multi-select this flips the highlighted row on/off (Space); for
    /// single-select it is a harmless no-op because the highlight already
    /// *is* the live selection.
    QuestionToggle,
    /// Move selection to previous action in the permission sheet (Left / BackTab).
    PermissionPrevOption,
    /// Move selection to next action in the permission sheet (Right / Tab).
    PermissionNextOption,
    /// Advance to the next question, or submit all answers from the final
    /// question (Enter).
    QuestionSubmit,
    /// Advance to the next question in a multi-question ask_user request (Tab / Right).
    QuestionNext,
    /// Return to the previous question (Shift+Tab / Left).
    QuestionPrevious,
    /// Cancel the question modal.
    QuestionCancel,
    /// ADR-0175: navigation/decision actions on the PreAttach
    /// interstitial surface. The four variants mirror the Question
    /// sheet's, but route to `PreAttachState::apply` instead of the
    /// Question sheet's `QuestionModel::update`, keeping the two
    /// surfaces' dispatch disjoint (no overloading of `Question*`
    /// actions).
    PreAttachUp,
    PreAttachDown,
    PreAttachToggle,
    PreAttachSubmit,
    PreAttachCancel,
    /// Submit the input-injection panel's typed text (L3.5 β).
    InputSubmit,
    /// Cancel the input-injection panel (run the command non-interactively).
    InputCancel,
    /// Select a question option by its 1-based index.
    QuestionSelect(usize),
    /// Insert a character into the question modal's "Other" free-text field.
    QuestionInsertChar(char),
    /// Delete a character from the question modal's "Other" free-text field.
    QuestionBackspace,
    /// Start selection at screen coordinates.
    SelectionStart {
        x: u16,
        y: u16,
    },
    /// Update selection to screen coordinates.
    SelectionUpdate {
        x: u16,
        y: u16,
    },
    /// End selection.
    SelectionEnd,
    /// Select entire block at coordinates (e.g. triple-click).
    SelectBlock {
        x: u16,
        y: u16,
    },
    /// Right-click at screen coordinates. Opens a context/detail view for the
    /// interactive element under the cursor (e.g. a tool step's full output).
    RightClick {
        x: u16,
        y: u16,
    },
    /// Mouse pointer moved to screen coordinates (hover tracking). Used to
    /// drive hover affordances on clickable elements like reasoning-trace
    /// headers. Suppressed while an overlay modal is open.
    Hover {
        x: u16,
        y: u16,
    },
    /// Open the `/btw` asides list modal (ADR-0103 §5). Mapped from F5.
    OpenBtwList,
    /// Open the global view quick switcher (ADR-0139, `C-x p`). A transient
    /// chooser over every browse surface: open views first in MRU order,
    /// then the rest as discovery. Esc closes it with nothing changed.
    ViewSwitcherToggle,
    /// Append a character to the switcher's fuzzy filter (phase 5). The
    /// row set narrows live; ↑/↓ walk the filtered rows.
    ViewSwitcherFilter {
        ch: char,
    },
    /// Drop the last character from the switcher's filter (Backspace).
    ViewSwitcherBackspace,
    /// Switch to the view highlighted in the quick switcher (ADR-0139,
    /// Enter). Hides the current browse view (state retained in the
    /// `PanelRegistry`) and focuses the target with its retained
    /// scroll/index restored.
    ViewSwitchActivate,
    /// Explicitly close the selected retained view and discard its UI state.
    /// Delete never removes the underlying session or backend resource.
    ViewCloseSelected,
    /// Jump into the aside highlighted in the asides modal (ADR-0103 §5).
    BtwFocusSelected,
    /// Close + discard the aside highlighted in the asides modal
    /// (`D`, ADR-0103 §5).
    BtwCloseSelected,
    /// Interrupt the viewed aside's in-flight round (Esc inside an aside
    /// view, ADR-0103 §2). Interrupting never closes the aside.
    InterruptSide,
    /// Toggle the in-dialog localized key reference overlay (`?` inside an active dialog).
    ToggleDialogKeys,
    /// Scroll the in-dialog localized key reference overlay (`↑`/`↓`/`PgUp`/`PgDn`).
    DialogKeysScroll {
        delta: i8,
    },
    /// Move to the previous sibling subagent task.
    PrevSibling,
    /// Move to the next sibling subagent task.
    NextSibling,
    /// Arm the `Ctrl+X` scene namespace, awaiting its second stroke. The router
    /// emits this for the opening stroke; re-arming is idempotent.
    SetSceneNamespaceArmed(bool),
    /// Cancel an armed scene namespace without acting (`Esc`, `C-g`, or an
    /// unrecognized second stroke).
    CancelSceneNamespace,
    /// Step back one level inside the active Scene — a dropdown, a drill-in
    /// pane, the dashboard's preview or inline prompt. Esc on a scene produces
    /// this (ADR-0298 §2). It never leaves the Scene: a Scene's own exit is
    /// [`InputAction::CloseScene`] (`C-x w` / `C-x k`), never the universal
    /// dismiss chord (ADR-0205 `[INV-TUI-CLEAN-02]`).
    SceneBack,
    /// Explicitly close/exit the active scene or dialog (e.g. via `C-x w`).
    CloseScene,
    /// Terminal was resized (SIGWINCH). The event loop forces a redraw and
    /// re-emits `EnableMouseCapture` so the crossterm parser's internal state
    /// machine is resynced: a resize frequently splits an in-flight SGR mouse
    /// sequence across `event::read()` boundaries, and crossterm then hands the
    /// leftover bytes back as spurious `KeyCode::Char` events (issue #854/#668).
    /// Re-arming capture is the cleanest way to get both sides back in step.
    TerminalResized {
        cols: u16,
        rows: u16,
    },
}

impl InputAction {
    /// Whether this action is a modal-opening command reached by typing a
    /// slash command into the composer (e.g. `/models`) — as opposed to a
    /// keybinding such as Ctrl+R (history).
    ///
    /// These commands consume the composer text (the typed `/cmd`) the same
    /// way `SendSlash` does, but unlike `SendSlash` they are intercepted
    /// locally and never forwarded to the harness. The text they consumed is
    /// therefore not carried on the action and would be lost for input-history
    /// purposes; the event loop snapshots the composer before dispatch and uses
    /// this predicate to decide whether to record it.
    pub fn is_text_modal_command(&self) -> bool {
        matches!(
            self,
            InputAction::OpenSessions
                | InputAction::OpenModels
                | InputAction::OpenConnections
                | InputAction::OpenPermissions
                | InputAction::OpenTools
                | InputAction::OpenMcp
                | InputAction::OpenSkills
                | InputAction::OpenConfig
        )
    }
}
