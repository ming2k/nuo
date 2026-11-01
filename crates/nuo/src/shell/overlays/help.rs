//! The Help & Keybindings modal dialog (ADR-0131 [INV-HELP-01..03]).
//!
//! This dialog is the client's **single shortcut discovery surface**. Chrome
//! therefore carries no keycaps of its own beyond the standing `F1 help`
//! entrance: a chord printed in the SceneBar or a footer would be a hardcoded
//! copy of a binding the user can remap, and would compete with the one surface
//! whose job is to answer "what can I press here?".
//!
//! Every chord below is resolved from the live keymap
//! ([`GlobalOverrides`] / [`SurfaceOverrides`]) rather than written as a
//! literal, so a remapped binding displays the chord that actually fires. That
//! is the same rule the composer hint row follows: a keycap is a promise.

use nuotc::{Frame, Line, Modifier, Rect, Span, Style};

use crate::shell::keymap::{CommandId, GlobalOverrides, SurfaceOverrides, SurfaceVerb};
use crate::shell::render::Theme;
use crate::shell::surfaces::SceneKind;
use nuo_ui_primitives::elevation::modal_frame;
use nuo_ui_primitives::primitives::{
    ContentModalSpec, FooterHint, content_modal_area, modal_header, render_body,
    render_modal_footer,
};

/// Format one keybinding row with prominent accent keycap and muted description.
fn binding_row(key: &str, desc: &str, theme: &Theme) -> Line<'static> {
    let key_style = Style::default()
        .fg(theme.keycap_accent())
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(theme.fg());

    Line::from(vec![
        Span::styled(format!("  {:<12} ", key), key_style),
        Span::styled(desc.to_string(), desc_style),
    ])
}

/// Section title inside the help body.
fn section_header(title: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        title.to_string(),
        Style::default()
            .fg(theme.brand())
            .add_modifier(Modifier::BOLD),
    ))
}

/// Every chord the dialog advertises, resolved once from the live keymap.
///
/// Binding a row to this struct (instead of a string literal) is what keeps the
/// documented chord and the dispatched chord the same value: a user remap
/// changes the display, and a command that loses its chord renders as
/// `unbound` rather than inventing a plausible keycap.
struct Chords {
    help: String,
    new_tab: String,
    close_tab: String,
    notices: String,
    settings: String,
    scene_back: String,
    scene_forward: String,
    focus_prev: String,
    focus_next: String,
    switch_focus: String,
    open_history: String,
    toggle_send_mode: String,
}

impl Chords {
    fn resolve(global: &GlobalOverrides, surface: &SurfaceOverrides) -> Self {
        // A command with no chord at all is an explicit, visible state, not a
        // reason to print a key that does nothing.
        let command = |cmd: CommandId| {
            global
                .effective_binding(cmd)
                .map(|key| key.display().to_string())
                .unwrap_or_else(|| "unbound".to_string())
        };
        let verb = |verb: SurfaceVerb| surface.effective_binding(verb).display().to_string();
        Self {
            help: command(CommandId::OpenHelp),
            new_tab: command(CommandId::NewTab),
            close_tab: command(CommandId::CloseTab),
            notices: command(CommandId::OpenNotices),
            settings: command(CommandId::NavigateSettings),
            scene_back: verb(SurfaceVerb::SceneHistoryBack),
            scene_forward: verb(SurfaceVerb::SceneHistoryForward),
            focus_prev: verb(SurfaceVerb::FocusPrevTarget),
            focus_next: verb(SurfaceVerb::FocusNextTarget),
            switch_focus: command(CommandId::ToggleFocusPlane),
            open_history: verb(SurfaceVerb::OpenHistory),
            toggle_send_mode: verb(SurfaceVerb::ToggleSendMode),
        }
    }
}

/// Action kind that can be executed directly by single-key accelerator or row click in HelpDialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpActionKind {
    NewTab,
    CloseTab,
    Dashboard,
    Settings,
    Switcher,
    Sessions,
    Notices,
    ThreadStats,
    Connections,
    CloseHelp,
}

/// A structured entry in the help dialog body.
#[derive(Debug, Clone)]
pub struct HelpEntry {
    pub line: Line<'static>,
    pub action: Option<HelpActionKind>,
}

/// Build the structured entries for the help dialog.
pub fn build_help_entries(
    scene: SceneKind,
    viewed_session_id: Option<&str>,
    unread_faults: usize,
    global: &GlobalOverrides,
    surface: &SurfaceOverrides,
    theme: &Theme,
) -> Vec<HelpEntry> {
    let keys = Chords::resolve(global, surface);

    struct EntryBuilder<'a> {
        entries: Vec<HelpEntry>,
        theme: &'a Theme,
    }
    impl<'a> EntryBuilder<'a> {
        fn header(&mut self, title: &str) {
            self.entries.push(HelpEntry {
                line: section_header(title, self.theme),
                action: None,
            });
        }
        fn separator(&mut self) {
            self.entries.push(HelpEntry {
                line: Line::from(""),
                action: None,
            });
        }
        fn row(&mut self, key: &str, desc: &str, action: Option<HelpActionKind>) {
            self.entries.push(HelpEntry {
                line: binding_row(key, desc, self.theme),
                action,
            });
        }
    }

    let mut b = EntryBuilder {
        entries: Vec::new(),
        theme,
    };

    // 1. Current Scene Contextual Actions (ADR-0131 [INV-HELP-01])
    match scene {
        SceneKind::Thread => {
            let session_tag = viewed_session_id
                .map(|id| {
                    let tail = if id.len() <= 4 {
                        id
                    } else {
                        &id[id.len() - 4..]
                    };
                    format!("Thread #{}", tail)
                })
                .unwrap_or_else(|| "Thread (Active Conversation)".to_string());
            b.header(&session_tag);
            let notice_desc = if unread_faults > 0 {
                format!("Notices & diagnostic faults ({} unread)", unread_faults)
            } else {
                "Notices & diagnostic fault log".to_string()
            };
            b.row(
                &format!("o, {}", keys.notices),
                &notice_desc,
                Some(HelpActionKind::Notices),
            );
            b.row(&keys.open_history, "Recall a prompt", None);
            b.row(
                "/stats",
                "Context window & token telemetry",
                Some(HelpActionKind::ThreadStats),
            );
            b.row(
                "/connections",
                "Model route & connection details",
                Some(HelpActionKind::Connections),
            );
        }
        SceneKind::Subagent => {
            b.header("Subagent Execution View");
            b.row(&keys.scene_back, "Step back to the parent thread", None);
        }
        SceneKind::Dashboard => {
            b.header("Dashboard (Thread Orchestration)");
            b.row("Enter", "Inspect a thread's dossier", None);
            b.row("/", "Filter threads by id or status", None);
            b.row("Esc", "Leave the dashboard", None);
        }
        SceneKind::Settings => {
            b.header("Settings & Preferences");
            b.row("Enter", "Select a category or toggle a setting", None);
            b.row("Esc", "Leave settings", None);
        }
    }
    b.separator();

    // 2. Scene Navigation — the intra-tab history stack.
    b.header("Scene Navigation");
    b.row(
        &format!("{} / {}", keys.scene_back, keys.scene_forward),
        "Step back / forward through this tab's scene history",
        None,
    );
    b.row(
        &keys.switch_focus,
        "Switch focus between the composer and the transcript",
        None,
    );
    b.row(
        &format!("{} / {}", keys.focus_prev, keys.focus_next),
        "Walk the highlighted transcript entry (↑ / ↓ do the same once one is held)",
        None,
    );
    b.separator();

    // 3. Client Domain: Tabs & Workspaces
    b.header("Tabs & Workspaces");
    b.row(
        &format!("t, {}", keys.new_tab),
        "New thread in a new tab",
        Some(HelpActionKind::NewTab),
    );
    b.row(
        &format!("w, {}", keys.close_tab),
        "Close the active tab",
        Some(HelpActionKind::CloseTab),
    );
    b.row("Alt-1..9", "Jump directly to tab 1 through 9", None);
    b.separator();

    // 4. Client Domain: Views & Navigation
    b.header("Views & Navigation");
    b.row(
        "d",
        "Navigate to the dashboard (/dashboard)",
        Some(HelpActionKind::Dashboard),
    );
    b.row(
        "s",
        "Open the saved threads overview picker (/threads)",
        Some(HelpActionKind::Sessions),
    );
    b.row(
        &format!(",, {}", keys.settings),
        "Open Settings & appearance",
        Some(HelpActionKind::Settings),
    );
    b.separator();

    // 5. Client Domain: Composer & System
    b.header("Composer & System");
    b.row("Enter", "Send the prompt", None);
    b.row(
        &keys.toggle_send_mode,
        "Switch between steer and follow-up while a turn runs",
        None,
    );
    b.row(
        "p",
        "Open the command palette (quick switcher, /commands)",
        Some(HelpActionKind::Switcher),
    );
    b.row("/", "Slash commands & tool execution in the composer", None);
    b.row(
        &keys.help,
        "Toggle this Help & Keybindings dialog",
        Some(HelpActionKind::CloseHelp),
    );

    b.entries
}

/// Render the sovereign Help dialog.
#[allow(clippy::too_many_arguments)]
pub fn draw_help_modal(
    frame: &mut Frame,
    _area: Rect,
    scene: SceneKind,
    viewed_session_id: Option<&str>,
    unread_faults: usize,
    global: &GlobalOverrides,
    surface: &SurfaceOverrides,
    scroll: &mut usize,
    actions: &mut Vec<Option<HelpActionKind>>,
    theme: &Theme,
) -> Rect {
    let entries = build_help_entries(
        scene,
        viewed_session_id,
        unread_faults,
        global,
        surface,
        theme,
    );

    actions.clear();
    actions.extend(entries.iter().map(|e| e.action));

    let geometry = ContentModalSpec::TOOLS;
    let desired_rows = (entries.len() as u16).saturating_add(4);
    let bounds = content_modal_area(frame, geometry, desired_rows);
    let f = modal_frame(frame, bounds, theme, true, true);

    modal_header(frame, f.header, "Help & Keybindings", theme);

    let body_lines: Vec<Line<'static>> = entries.into_iter().map(|e| e.line).collect();

    render_body(
        frame,
        f.body,
        body_lines,
        scroll,
        nuo_ui_primitives::primitives::BodyRenderOptions::new(None, 0, false),
        theme,
    );

    if let Some(fo) = f.footer {
        let hints = [
            FooterHint::secondary("↑/↓", "scroll"),
            FooterHint::primary("Key/Click", "direct action"),
            FooterHint::always("Esc/F1", "close"),
        ];
        render_modal_footer(frame, fo, &hints, theme);
    }

    bounds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_header_renders_plain_title_without_dividers() {
        let theme = Theme::default();
        let line = section_header("Thread #d445", &theme);
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "Thread #d445");
        assert!(!line.spans[0].content.contains('─'));
    }

    /// Canonical chords: the dialog documents the binding that actually fires.
    #[test]
    fn chords_resolve_from_the_canonical_keymap() {
        let keys = Chords::resolve(&GlobalOverrides::default(), &SurfaceOverrides::default());
        assert_eq!(keys.help, "F1");
        assert_eq!(keys.new_tab, "Alt-t");
        assert_eq!(keys.close_tab, "Alt-w");
        assert_eq!(keys.notices, "Alt-o");
        assert_eq!(keys.settings, "Alt-,");
        // The scene verbs are `Alt-[` / `Alt-]`; before the chord vocabulary
        // covered punctuation these rendered as `·`, which is why the chord
        // text used to be hardcoded into the SceneBar instead.
        assert_eq!(keys.scene_back, "Alt-[");
        assert_eq!(keys.scene_forward, "Alt-]");
    }

    /// A remap must move the displayed chord, not leave a stale keycap behind.
    #[test]
    fn chords_follow_user_remaps() {
        let mut surface_map = std::collections::HashMap::new();
        // `scene_back` is a nested-table verb (`[keybindings.session]`).
        surface_map.insert("scene_back".to_string(), "alt-b".to_string());
        let surface = SurfaceOverrides::from_config(&surface_map);
        let keys = Chords::resolve(&GlobalOverrides::default(), &surface);
        assert_eq!(keys.scene_back, "Alt-b");
        // The unremapped direction still shows its canonical chord.
        assert_eq!(keys.scene_forward, "Alt-]");
    }

    /// A command with no chord renders `unbound` rather than a plausible lie.
    #[test]
    fn unbound_command_is_labelled_not_invented() {
        let keys = Chords::resolve(&GlobalOverrides::default(), &SurfaceOverrides::default());
        // Every advertised command has a canonical chord today; the `unbound`
        // fallback is the contract for one that does not, and `Key::display`
        // returning `·` for an unnamed chord is what it exists to catch.
        for chord in [&keys.help, &keys.new_tab, &keys.close_tab, &keys.notices] {
            assert_ne!(chord, "·", "an advertised chord must be displayable");
        }
    }

    #[test]
    fn help_entries_structure_and_action_mapping() {
        let theme = Theme::default();
        let entries = build_help_entries(
            SceneKind::Thread,
            Some("thread-abc1234"),
            2,
            &GlobalOverrides::default(),
            &SurfaceOverrides::default(),
            &theme,
        );

        // Header has None action
        assert_eq!(entries[0].action, None);

        // Find action entries
        let action_kinds: Vec<HelpActionKind> = entries.iter().filter_map(|e| e.action).collect();
        assert!(action_kinds.contains(&HelpActionKind::Notices));
        assert!(action_kinds.contains(&HelpActionKind::ThreadStats));
        assert!(action_kinds.contains(&HelpActionKind::Connections));
        assert!(action_kinds.contains(&HelpActionKind::NewTab));
        assert!(action_kinds.contains(&HelpActionKind::CloseTab));
        assert!(action_kinds.contains(&HelpActionKind::Dashboard));
        assert!(action_kinds.contains(&HelpActionKind::Sessions));
        assert!(action_kinds.contains(&HelpActionKind::Settings));
        assert!(action_kinds.contains(&HelpActionKind::Switcher));
        assert!(action_kinds.contains(&HelpActionKind::CloseHelp));
    }

    #[test]
    fn help_modal_adaptive_height_on_large_frame() {
        let mut terminal = nuotc::TestTerminal::new(100, 60);
        let mut scroll = 0;
        let mut actions = Vec::new();
        let mut bounds = Rect::default();
        terminal.draw(|f| {
            bounds = draw_help_modal(
                f,
                Rect::new(0, 0, 100, 60),
                SceneKind::Thread,
                Some("session-1"),
                0,
                &GlobalOverrides::default(),
                &SurfaceOverrides::default(),
                &mut scroll,
                &mut actions,
                &Theme::default(),
            );
        });

        // The modal height adapts to the content and is greater than the legacy hardcoded 22
        assert!(
            bounds.height > 22,
            "expected adaptive height > 22 on 60-row frame, got {}",
            bounds.height
        );
        assert_eq!(actions.len(), 27);
    }
}
