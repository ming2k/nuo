//! Palette invariants (RFC-0005 §3).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashSet;

use graticule::event::{
    Event, KeyCode as CtCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers as CtMods,
};
use view::model::layout::{InteractiveTarget, InteractiveTargetKind};
use view::model::selection::SelectionDrag;

use super::context::{PaletteContext, TabRef};
use super::entry::{Effect, Section};
use super::rank::Row;
use super::recents::Recents;
use super::verbs::{Requires, Verb, dialog_verbs, scene_verbs};
use super::{Projection, project_with};
use crate::shell::focus::FocusPlane;
use crate::shell::input::{Dispatch, InputAction, route_event};
use crate::shell::keymap::{Availability, CommandId, Key, KeyCode, KeyModifiers};
use crate::shell::modal_keys::ModalKeys;
use crate::shell::session::SceneKeys;
use crate::shell::sheet::SheetKeys;
use crate::shell::surfaces::{DialogKind, OverlaySurface, SceneKind};

const SCENES: [SceneKind; 6] = [
    SceneKind::Thread,
    SceneKind::Subagent,
    SceneKind::CommandLog,
    SceneKind::Diff,
    SceneKind::Dashboard,
    SceneKind::Settings,
];

fn all_dialogs() -> impl Iterator<Item = DialogKind> {
    DialogKind::ALL.into_iter().chain([DialogKind::Palette])
}

fn ctx_in(scene: SceneKind) -> PaletteContext {
    let mut ctx = PaletteContext::default();
    ctx.state.scene = scene;
    ctx.state.has_session = true;
    ctx
}

fn over(scene: SceneKind, dialog: DialogKind) -> PaletteContext {
    let mut ctx = ctx_in(scene);
    ctx.state.has_overlay = true;
    ctx.state.active_dialog = Some(dialog);
    ctx
}

fn browse(ctx: &PaletteContext) -> Projection {
    project_with(ctx, "", &Recents::default())
}

fn keys(p: &Projection) -> Vec<&str> {
    p.entries.iter().map(|r| r.entry.key.as_ref()).collect()
}

fn find<'a>(p: &'a Projection, key: &str) -> &'a super::rank::Ranked {
    p.entries
        .iter()
        .find(|r| r.entry.key == key)
        .unwrap_or_else(|| panic!("{key} missing from {:?}", keys(p)))
}

// [INV-PAL-CTX-02]: keycap honesty

/// The terminal event a user produces for `key`.
fn event_for(key: Key) -> Event {
    let code = match key.code {
        KeyCode::Char(c) => CtCode::Char(c),
        KeyCode::Enter => CtCode::Enter,
        KeyCode::Esc => CtCode::Esc,
        KeyCode::Tab => CtCode::Tab,
        KeyCode::BackTab => CtCode::BackTab,
        KeyCode::Backspace => CtCode::Backspace,
        KeyCode::Delete => CtCode::Delete,
        KeyCode::Up => CtCode::Up,
        KeyCode::Down => CtCode::Down,
        KeyCode::Left => CtCode::Left,
        KeyCode::Right => CtCode::Right,
        KeyCode::Home => CtCode::Home,
        KeyCode::End => CtCode::End,
        KeyCode::PageUp => CtCode::PageUp,
        KeyCode::PageDown => CtCode::PageDown,
        KeyCode::F(n) => CtCode::F(n),
        KeyCode::Other => panic!("unrepresentable key"),
    };
    let mut modifiers = CtMods::NONE;
    for (ours, theirs) in [
        (KeyModifiers::SHIFT, CtMods::SHIFT),
        (KeyModifiers::CONTROL, CtMods::CONTROL),
        (KeyModifiers::ALT, CtMods::ALT),
        (KeyModifiers::SUPER, CtMods::SUPER),
    ] {
        if key.modifiers.contains(ours) {
            modifiers |= theirs;
        }
    }
    // A capital letter arrives with Shift held, the way terminals report it.
    if matches!(key.code, KeyCode::Char(c) if c.is_ascii_uppercase()) {
        modifiers |= CtMods::SHIFT;
    }
    Event::Key(KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

/// Route `verb`'s keycap through the real router, in the key state its
/// `requires` describes.
fn resolve(verb: &Verb, scene: SceneKind, overlay: Option<OverlaySurface>) -> InputAction {
    let key = verb.keycap.expect("verb without keycap");
    let mut dispatch = Dispatch {
        overlay,
        scene,
        ..Default::default()
    };
    let mut scene_keys = SceneKeys::default();
    let mut modal_keys = ModalKeys::default();
    match verb.requires {
        Requires::Nothing | Requires::ComposerPlane => {}
        Requires::Running => scene_keys.is_responding = true,
        Requires::TranscriptPlane => {
            dispatch.focus = FocusPlane::Transcript;
            scene_keys.focus = FocusPlane::Transcript;
        }
        Requires::FocusedEntry => {
            dispatch.focus = FocusPlane::Transcript;
            scene_keys.focus = FocusPlane::Transcript;
            scene_keys.entry_focus = Some(InteractiveTarget {
                message_idx: 0,
                block_idx: 0,
                kind: InteractiveTargetKind::ToolStep,
            });
        }
        Requires::SettingsDetail => {
            modal_keys.config_focus = crate::shell::overlays::ConfigFocus::Detail;
        }
    }
    let mut input = String::new();
    let mut cursor = 0;
    route_event(
        event_for(key),
        &mut input,
        &mut cursor,
        dispatch,
        &modal_keys,
        &SheetKeys::default(),
        &scene_keys,
        &mut SelectionDrag::default(),
    )
}

#[test]
fn every_scene_keycap_resolves_to_its_declared_action() {
    for scene in SCENES {
        for verb in scene_verbs(scene).iter().flat_map(|group| group.iter()) {
            assert_eq!(
                resolve(verb, scene, None),
                verb.action,
                "{} ({}) in {scene:?}",
                verb.key,
                verb.keycap.unwrap().display()
            );
        }
    }
}

#[test]
fn every_dialog_keycap_resolves_to_its_declared_action() {
    for dialog in all_dialogs() {
        for verb in dialog_verbs(dialog) {
            assert_eq!(
                resolve(
                    verb,
                    SceneKind::Thread,
                    Some(OverlaySurface::Dialog(dialog))
                ),
                verb.action,
                "{} ({}) in {dialog:?}",
                verb.key,
                verb.keycap.unwrap().display()
            );
        }
    }
}

#[test]
fn every_tab_keycap_resolves_to_its_tab() {
    let mut ctx = ctx_in(SceneKind::Thread);
    ctx.tabs = (0..3)
        .map(|index| TabRef {
            index,
            label: format!("t{index}"),
            key: format!("tab:{index}"),
            active: index == 0,
        })
        .collect();
    let p = browse(&ctx);
    let tabs: Vec<_> = p
        .entries
        .iter()
        .filter(|r| r.entry.section == Section::Tabs)
        .collect();
    assert_eq!(tabs.len(), 2, "the active tab is not offered");
    for ranked in tabs {
        let Effect::Action(InputAction::SelectTab(index)) = ranked.entry.effect else {
            panic!("tab entry must select its tab");
        };
        let mut input = String::new();
        let mut cursor = 0;
        let routed = route_event(
            event_for(ranked.entry.keycap.unwrap()),
            &mut input,
            &mut cursor,
            Dispatch::default(),
            &ModalKeys::default(),
            &SheetKeys::default(),
            &SceneKeys::default(),
            &mut SelectionDrag::default(),
        );
        assert_eq!(routed, InputAction::SelectTab(index));
    }
}

#[test]
fn no_keycap_renders_as_an_unknown_glyph() {
    for ctx in SCENES
        .into_iter()
        .map(ctx_in)
        .chain(all_dialogs().map(|d| over(SceneKind::Thread, d)))
    {
        for ranked in &browse(&ctx).entries {
            if let Some(key) = ranked.entry.keycap {
                assert_ne!(key.display(), "?", "{}", ranked.entry.key);
            }
        }
    }
}

#[test]
fn entry_keys_are_unique_in_every_context() {
    for ctx in SCENES
        .into_iter()
        .map(ctx_in)
        .chain(all_dialogs().map(|d| over(SceneKind::Thread, d)))
    {
        let p = browse(&ctx);
        let mut seen = HashSet::new();
        for key in keys(&p) {
            assert!(seen.insert(key), "duplicate entry key {key}");
        }
    }
}

// [INV-PAL-CTX-04]: relevance and availability

#[test]
fn the_current_scene_leads_and_other_scenes_are_absent() {
    let p = browse(&ctx_in(SceneKind::Diff));
    assert_eq!(
        p.rows.first(),
        Some(&Row::Header(Section::Scene(SceneKind::Diff)))
    );
    assert_eq!(
        find(&p, "diff.next-file").entry.keycap,
        Some(Key::plain('n'))
    );
    let ks = keys(&p);
    assert!(
        !ks.iter()
            .any(|k| k.starts_with("chat.") || k.starts_with("dashboard."))
    );

    let thread = browse(&ctx_in(SceneKind::Thread));
    let ks = keys(&thread);
    assert!(ks.contains(&"thread.search") && ks.contains(&"chat.prev-entry"));
    assert!(!ks.iter().any(|k| k.starts_with("diff.")));
}

#[test]
fn over_a_dialog_its_verbs_lead_and_scene_verbs_step_aside() {
    let p = browse(&over(SceneKind::Thread, DialogKind::Models));
    assert_eq!(
        p.rows.first(),
        Some(&Row::Header(Section::Dialog(DialogKind::Models)))
    );
    assert_eq!(
        find(&p, "models.favorite").entry.effect,
        Effect::Action(InputAction::ProviderPickerToggleFavorite)
    );
    assert!(
        !p.entries
            .iter()
            .any(|r| matches!(r.entry.section, Section::Scene(_)))
    );
}

#[test]
fn blocked_entries_stay_visible_with_a_reason_after_runnable_ones() {
    let idle = browse(&ctx_in(SceneKind::Thread));
    let interrupt = find(&idle, "thread.interrupt");
    assert_eq!(
        interrupt.entry.availability,
        Availability::Unavailable(Requires::Running.reason())
    );
    let scene: Vec<_> = idle
        .entries
        .iter()
        .filter(|r| r.group == Section::Scene(SceneKind::Thread))
        .collect();
    let first_blocked = scene.iter().position(|r| !r.entry.is_available()).unwrap();
    assert!(
        scene[first_blocked..]
            .iter()
            .all(|r| !r.entry.is_available()),
        "a runnable entry sorted after a blocked one"
    );

    let mut running = ctx_in(SceneKind::Thread);
    running.state.is_responding = true;
    assert!(
        find(&browse(&running), "thread.interrupt")
            .entry
            .is_available()
    );
}

#[test]
fn going_where_you_already_are_says_so() {
    let p = browse(&ctx_in(SceneKind::Settings));
    assert_eq!(
        find(&p, "cmd.NavigateSettings").entry.availability,
        Availability::Unavailable("you are here")
    );
    assert!(find(&p, "cmd.NavigateDashboard").entry.is_available());
}

#[test]
fn composer_internals_and_dead_rows_are_not_offered() {
    let p = browse(&ctx_in(SceneKind::Thread));
    for key in ["cmd.SendPrompt", "cmd.QueueFollowUp", "cmd.CommandPalette"] {
        assert!(!keys(&p).contains(&key), "{key} must not be in the palette");
    }
    for key in [
        "cmd.OpenModels",
        "cmd.OpenConnections",
        "cmd.Quit",
        "cmd.NewTab",
    ] {
        find(&p, key);
    }
}

// Query and ranking

#[test]
fn facets_scope_the_entry_set() {
    let ctx = over(SceneKind::Thread, DialogKind::Queue);
    let recents = Recents::default();
    let here = project_with(&ctx, ":", &recents);
    assert!(!here.entries.is_empty());
    assert!(here.entries.iter().all(|r| r.entry.section.is_here()));

    let places = project_with(&ctx, "@", &recents);
    assert!(places.entries.iter().all(|r| r.entry.section.is_place()));
    find(&places, "cmd.NavigateDashboard");

    let actions = project_with(&ctx, "> quit", &recents);
    assert!(actions.entries.iter().all(|r| !r.entry.section.is_place()));
    assert_eq!(actions.entries[0].entry.key, "cmd.Quit");
}

#[test]
fn a_title_match_ranks_first_and_is_highlighted() {
    let p = project_with(&ctx_in(SceneKind::Diff), "next file", &Recents::default());
    let top = &p.entries[0];
    assert_eq!(top.entry.key, "diff.next-file");
    assert!(!top.positions.is_empty());
    assert!(
        p.rows.iter().all(|row| matches!(row, Row::Entry(_))),
        "search is flat"
    );
}

#[test]
fn a_slash_alias_finds_its_command_without_highlighting() {
    let p = project_with(&ctx_in(SceneKind::Thread), "/stats", &Recents::default());
    let hit = find(&p, "cmd.OpenThreadStats");
    assert!(hit.positions.is_empty());
}

#[test]
fn recent_entries_are_lifted_after_the_context() {
    let ctx = ctx_in(SceneKind::Thread);
    let mut recents = Recents::default();
    recents.touch("cmd.OpenModels");
    let p = project_with(&ctx, "", &recents);
    let recent_header = p
        .rows
        .iter()
        .position(|r| *r == Row::Header(Section::Recent))
        .expect("recent group");
    let scene_header = p
        .rows
        .iter()
        .position(|r| *r == Row::Header(Section::Scene(SceneKind::Thread)))
        .unwrap();
    assert!(scene_header < recent_header);
    let Row::Entry(first_recent) = p.rows[recent_header + 1] else {
        panic!("header followed by header");
    };
    assert_eq!(p.entries[first_recent].entry.key, "cmd.OpenModels");
    assert_eq!(
        p.entries[first_recent].entry.effect,
        Effect::Command(CommandId::OpenModels)
    );
}

#[test]
fn headers_are_not_addressable_entries() {
    let p = browse(&ctx_in(SceneKind::Thread));
    assert_eq!(p.entry_at_row(0), None);
    assert_eq!(p.row_of(0), Some(1));
    assert_eq!(p.entry_at_row(1), Some(0));
    assert_eq!(
        p.selected(10_000).map(|r| &r.entry.key),
        p.entries.first().map(|r| &r.entry.key)
    );
}

// View

fn draw(width: u16, height: u16, query: &str, ctx: &PaletteContext) -> String {
    let projection = project_with(ctx, query, &Recents::default());
    let theme = crate::shell::render::Theme::default();
    let mut terminal = graticule::TestTerminal::new(width, height);
    let mut scroll = 0;
    terminal.draw(|f| {
        super::view::draw(
            f,
            super::view::PaletteProps {
                query,
                cursor_position: query.len(),
                projection: &projection,
                selected: 0,
                scroll: &mut scroll,
                show_caret: true,
            },
            &theme,
            &Default::default(),
            &mut Default::default(),
        );
    });
    terminal
        .buffer()
        .cells()
        .iter()
        .map(|c| c.symbol.as_str())
        .collect()
}

#[test]
fn the_view_names_the_context_and_explains_the_selection() {
    let screen = draw(100, 30, "", &ctx_in(SceneKind::Diff));
    assert!(screen.contains("Commands · Diff viewer"), "{screen}");
    assert!(screen.contains("Next file"));
    assert!(
        screen.contains("Jump to the next changed file"),
        "detail line"
    );
}

#[test]
fn the_view_survives_tiny_terminals_and_empty_results() {
    let ctx = over(SceneKind::Thread, DialogKind::Connections);
    draw(40, 12, "", &ctx);
    draw(24, 8, "zzzzzz", &ctx);
    let screen = draw(80, 24, "zzzzzz", &ctx);
    assert!(screen.contains("Nothing matches"));
}

#[test]
fn the_palette_chord_reaches_the_palette_from_every_scene_and_dialog() {
    let route = |scene: SceneKind, overlay: Option<OverlaySurface>| {
        let mut input = String::new();
        let mut cursor = 0;
        route_event(
            event_for(Key::CTRL_SHIFT_P),
            &mut input,
            &mut cursor,
            Dispatch {
                overlay,
                scene,
                ..Default::default()
            },
            &ModalKeys::default(),
            &SheetKeys::default(),
            &SceneKeys::default(),
            &mut SelectionDrag::default(),
        )
    };
    for scene in SCENES {
        assert_eq!(route(scene, None), InputAction::PaletteToggle, "{scene:?}");
    }
    for dialog in DialogKind::ALL {
        assert_eq!(
            route(SceneKind::Thread, Some(OverlaySurface::Dialog(dialog))),
            InputAction::PaletteToggle,
            "{dialog:?}"
        );
    }
}

// [INV-PAL-CTX-03]: end to end through the real dispatcher

async fn run(app: &mut crate::shell::App, query: &str) {
    use crate::shell::event_loop::actions::dispatch_action_for_test as dispatch;
    let runtime = crate::shell::event_loop::runtime::UiRuntime::minimal_for_test();
    dispatch(app, &runtime, InputAction::PaletteToggle, "s1").await;
    assert_eq!(app.active_dialog(), Some(DialogKind::Palette));
    for ch in query.chars() {
        dispatch(app, &runtime, InputAction::PaletteInsert { ch }, "s1").await;
    }
    dispatch(app, &runtime, InputAction::PaletteRun, "s1").await;
}

#[tokio::test]
async fn running_a_command_entry_closes_the_palette_and_executes_it() {
    let mut app = crate::shell::tests::new_app_for_relay_tests();
    run(&mut app, "switch model").await;
    assert_eq!(app.active_dialog(), Some(DialogKind::Models));
    assert_eq!(
        app.surfaces.palette_recents.position("cmd.OpenModels"),
        Some(0)
    );
}

#[tokio::test]
async fn running_a_verb_entry_dispatches_the_action_its_key_produces() {
    let mut app = crate::shell::tests::new_app_for_relay_tests();
    assert!(!app.threads.search.active);
    run(&mut app, ":search transcript").await;
    assert_eq!(app.active_dialog(), None, "the palette closed");
    assert!(app.threads.search.active, "Ctrl-f's action ran");
    assert_eq!(
        app.surfaces.palette_recents.position("thread.search"),
        Some(0)
    );
}

#[tokio::test]
async fn running_a_blocked_entry_explains_and_keeps_the_palette_open() {
    let mut app = crate::shell::tests::new_app_for_relay_tests();
    run(&mut app, ":interrupt the running round").await;
    assert_eq!(app.active_dialog(), Some(DialogKind::Palette));
    assert_eq!(
        app.surfaces.palette_recents.position("thread.interrupt"),
        None
    );
}
