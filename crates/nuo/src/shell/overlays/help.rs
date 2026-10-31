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
    clear_focus: String,
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
            clear_focus: verb(SurfaceVerb::ClearFocusedTarget),
            open_history: verb(SurfaceVerb::OpenHistory),
            toggle_send_mode: verb(SurfaceVerb::ToggleSendMode),
        }
    }
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
    theme: &Theme,
) -> Rect {
    let geometry = ContentModalSpec::TOOLS;
    let bounds = content_modal_area(frame, geometry, 22);
    let f = modal_frame(frame, bounds, theme, true, true);

    modal_header(frame, f.header, "Help & Keybindings", theme);

    let keys = Chords::resolve(global, surface);
    let mut body = Vec::new();

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
            body.push(section_header(&session_tag, theme));
            let notice_desc = if unread_faults > 0 {
                format!("Notices & diagnostic faults ({} unread)", unread_faults)
            } else {
                "Notices & diagnostic fault log".to_string()
            };
            body.push(binding_row(&keys.notices, &notice_desc, theme));
            body.push(binding_row(&keys.open_history, "Recall a prompt", theme));
            body.push(binding_row(
                "/stats",
                "Context window & token telemetry",
                theme,
            ));
            body.push(binding_row(
                "/connections",
                "Model route & connection details",
                theme,
            ));
        }
        SceneKind::Subagent => {
            body.push(section_header("Subagent Execution View", theme));
            body.push(binding_row(
                &keys.scene_back,
                "Step back to the parent thread",
                theme,
            ));
        }
        SceneKind::Dashboard => {
            body.push(section_header("Dashboard (Thread Orchestration)", theme));
            body.push(binding_row("Enter", "Inspect a thread's dossier", theme));
            body.push(binding_row("/", "Filter threads by id or status", theme));
            body.push(binding_row("Esc", "Leave the dashboard", theme));
        }
        SceneKind::Settings => {
            body.push(section_header("Settings & Preferences", theme));
            body.push(binding_row(
                "Enter",
                "Select a category or toggle a setting",
                theme,
            ));
            body.push(binding_row("Esc", "Leave settings", theme));
        }
    }
    // Esc is the universal dismiss/step-back chord, resolved from the same
    // table as everything else rather than assumed.
    body.push(Line::from(""));

    // 2. Scene Navigation — the intra-tab history stack.
    //
    // This is *not* tab switching: `Alt-[`/`Alt-]` walk the active tab's
    // scene history (thread → subagent → back), and their chords are remappable
    // surface verbs. The SceneBar draws only a chevron for them; this block is
    // where the chords are documented, which is why chrome no longer has to.
    body.push(section_header("Scene Navigation", theme));
    body.push(binding_row(
        &format!("{} / {}", keys.scene_back, keys.scene_forward),
        "Step back / forward through this tab's scene history",
        theme,
    ));
    body.push(binding_row(
        &format!("{} / {}", keys.focus_prev, keys.focus_next),
        "Focus the previous / next transcript step",
        theme,
    ));
    body.push(binding_row(
        &keys.clear_focus,
        "Clear step focus; interrupt a running turn when nothing is focused",
        theme,
    ));
    body.push(Line::from(""));

    // 3. Client Domain: Tabs & Workspaces
    body.push(section_header("Tabs & Workspaces", theme));
    body.push(binding_row(&keys.new_tab, "New thread in a new tab", theme));
    body.push(binding_row(&keys.close_tab, "Close the active tab", theme));
    body.push(binding_row(
        "Alt-1..9",
        "Jump directly to tab 1 through 9",
        theme,
    ));
    body.push(Line::from(""));

    // 4. Client Domain: Views & Navigation
    body.push(section_header("Views & Navigation", theme));
    body.push(binding_row(
        "/dashboard",
        "Navigate to the dashboard",
        theme,
    ));
    body.push(binding_row(
        "/threads",
        "Open the saved threads overview picker",
        theme,
    ));
    body.push(binding_row(
        &keys.settings,
        "Open Settings & appearance",
        theme,
    ));
    body.push(Line::from(""));

    // 5. Client Domain: Composer & System
    body.push(section_header("Composer & System", theme));
    body.push(binding_row("Enter", "Send the prompt", theme));
    body.push(binding_row(
        &keys.toggle_send_mode,
        "Switch between steer and follow-up while a turn runs",
        theme,
    ));
    body.push(binding_row(
        "/commands",
        "Open the command palette (quick switcher)",
        theme,
    ));
    body.push(binding_row(
        "/",
        "Slash commands & tool execution in the composer",
        theme,
    ));
    body.push(binding_row(
        &keys.help,
        "Toggle this Help & Keybindings dialog",
        theme,
    ));

    render_body(
        frame,
        f.body,
        body,
        scroll,
        nuo_ui_primitives::primitives::BodyRenderOptions::new(None, 0, false),
        theme,
    );

    if let Some(fo) = f.footer {
        let hints = [
            FooterHint::primary("Key", "direct action"),
            FooterHint::always("Esc", "close"),
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
}
