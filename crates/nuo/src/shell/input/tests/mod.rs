//! The input test suite, split by interaction concern. Shared key-event
//! builders live here; per-concern groups are sibling modules.

// Integration test target: assertions fail by panicking, so the production
// no-panic lints are meaningless here (the exemption ends at the test boundary).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::shell::modal_keys::ModalKeys;
use crate::shell::session::SceneKeys;
use crate::shell::sheet::SheetKeys;
use graticule::event::{KeyEvent, KeyEventKind, KeyEventState};

fn enter(input: &mut String, exact: bool) -> InputAction {
    let mut cursor = input.chars().count();
    let mut drag = SelectionDrag::default();
    route_event(
        Event::Key(KeyEvent {
            code: KeyCode::Enter,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        input,
        &mut cursor,
        Dispatch::default(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys {
            has_exact_suggestion: exact,
            ..Default::default()
        },
        &mut drag,
    )
}

fn enter_with_completion(
    input: &mut String,
    kind: crate::shell::CompletionKind,
    suggestion_count: usize,
    suggestion_index: Option<usize>,
    has_exact_suggestion: bool,
) -> InputAction {
    let mut cursor = input.chars().count();
    let mut drag = SelectionDrag::default();
    route_event(
        Event::Key(KeyEvent {
            code: KeyCode::Enter,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        input,
        &mut cursor,
        Dispatch::default(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys {
            completion_kind: kind,
            suggestion_count,
            has_exact_suggestion,
            suggestion_index,
            ..Default::default()
        },
        &mut drag,
    )
}

fn enter_shell(input: &mut String) -> InputAction {
    let mut cursor = input.chars().count();
    let mut drag = SelectionDrag::default();
    route_event(
        Event::Key(KeyEvent {
            code: KeyCode::Enter,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        input,
        &mut cursor,
        Dispatch::default(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    )
}

fn key_in_view(code: KeyCode, in_subagent_view: bool, input: &mut String) -> InputAction {
    key_in_view_with(code, input, move |dispatch| {
        // Surface dispatch keys off the explicit scene, not the
        // legacy flags.
        dispatch.scene = if in_subagent_view {
            crate::shell::surfaces::SceneKind::Subagent
        } else {
            crate::shell::surfaces::SceneKind::Thread
        };
    })
}

fn key_with_mod_in_view(
    code: KeyCode,
    modifiers: KeyModifiers,
    in_subagent_view: bool,
    input: &mut String,
) -> InputAction {
    let mut cursor = input.chars().count();
    let mut drag = SelectionDrag::default();
    let dispatch = Dispatch {
        scene: if in_subagent_view {
            crate::shell::surfaces::SceneKind::Subagent
        } else {
            crate::shell::surfaces::SceneKind::Thread
        },
        ..Default::default()
    };
    route_event(
        Event::Key(graticule::event::KeyEvent::new(code, modifiers)),
        input,
        &mut cursor,
        dispatch,
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    )
}

fn key_in_view_with(
    code: KeyCode,
    input: &mut String,
    tune: impl FnOnce(&mut Dispatch),
) -> InputAction {
    let mut cursor = input.chars().count();
    let mut drag = SelectionDrag::default();
    let mut dispatch = Dispatch::default();
    tune(&mut dispatch);
    route_event(
        Event::Key(graticule::event::KeyEvent::new(code, KeyModifiers::NONE)),
        input,
        &mut cursor,
        dispatch,
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    )
}

fn key_with_focus(code: KeyCode) -> InputAction {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    route_event(
        Event::Key(graticule::event::KeyEvent::new(code, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        Dispatch {
            focus: crate::shell::focus::FocusPlane::Transcript,
            ..Default::default()
        },
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys {
            focus: crate::shell::focus::FocusPlane::Transcript,
            entry_focus: Some(view::model::layout::InteractiveTarget::tool_step(0)),
            ..Default::default()
        },
        &mut drag,
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum SurfaceFixture {
    None,
    UsageStats,
    Config,
    ThreadStats,
    SessionTrace,
    ContextPlan,
    Sessions,
    Queue,
    HistorySearch,
    Models,
    Connections,
    Skills,
    Tools,
    Mcp,
    ProviderPreset,
    CustomProvider,
    OauthPending,
    ModelEditor,
    Permissions,
}

impl SurfaceFixture {
    pub fn to_dispatch(
        self,
    ) -> (
        Option<crate::shell::surfaces::OverlaySurface>,
        crate::shell::surfaces::SceneKind,
    ) {
        use crate::shell::surfaces::{DialogKind, OverlaySurface, SceneKind, SheetKind};
        match self {
            Self::None => (None, SceneKind::Thread),
            Self::Config => (None, SceneKind::Settings),
            Self::UsageStats => (
                Some(OverlaySurface::Dialog(DialogKind::UsageStats)),
                SceneKind::Thread,
            ),
            Self::ThreadStats => (
                Some(OverlaySurface::Dialog(DialogKind::ThreadStats)),
                SceneKind::Thread,
            ),
            Self::SessionTrace => (
                Some(OverlaySurface::Dialog(DialogKind::SessionTrace)),
                SceneKind::Thread,
            ),
            Self::ContextPlan => (
                Some(OverlaySurface::Dialog(DialogKind::ContextPlan)),
                SceneKind::Thread,
            ),
            Self::Sessions => (
                Some(OverlaySurface::Dialog(DialogKind::Sessions)),
                SceneKind::Thread,
            ),
            Self::Queue => (
                Some(OverlaySurface::Dialog(DialogKind::Queue)),
                SceneKind::Thread,
            ),
            Self::HistorySearch => (
                Some(OverlaySurface::Dialog(DialogKind::HistorySearch)),
                SceneKind::Thread,
            ),
            Self::Models => (
                Some(OverlaySurface::Dialog(DialogKind::Models)),
                SceneKind::Thread,
            ),
            Self::Connections => (
                Some(OverlaySurface::Dialog(DialogKind::Connections)),
                SceneKind::Thread,
            ),
            Self::Skills => (
                Some(OverlaySurface::Dialog(DialogKind::Skills)),
                SceneKind::Thread,
            ),
            Self::Tools => (
                Some(OverlaySurface::Dialog(DialogKind::Tools)),
                SceneKind::Thread,
            ),
            Self::Mcp => (
                Some(OverlaySurface::Dialog(DialogKind::Mcp)),
                SceneKind::Thread,
            ),
            Self::Permissions => (
                Some(OverlaySurface::Dialog(DialogKind::Permissions)),
                SceneKind::Thread,
            ),
            Self::ProviderPreset => (
                Some(OverlaySurface::Sheet(SheetKind::ProviderPreset)),
                SceneKind::Thread,
            ),
            Self::CustomProvider => (
                Some(OverlaySurface::Sheet(SheetKind::CustomProvider)),
                SceneKind::Thread,
            ),
            Self::OauthPending => (
                Some(OverlaySurface::Sheet(SheetKind::OAuthPending)),
                SceneKind::Thread,
            ),
            Self::ModelEditor => (
                Some(OverlaySurface::Sheet(SheetKind::ModelEditor)),
                SceneKind::Thread,
            ),
        }
    }
}

fn run_key(
    input: &mut String,
    cursor: &mut usize,
    code: KeyCode,
    modifiers: KeyModifiers,
    fixture: SurfaceFixture,
    has_focus: bool,
) -> InputAction {
    let (overlay, scene) = fixture.to_dispatch();
    let mut drag = SelectionDrag::default();
    route_event(
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        input,
        cursor,
        Dispatch {
            overlay,
            scene,
            focus: if has_focus {
                crate::shell::focus::FocusPlane::Transcript
            } else {
                crate::shell::focus::FocusPlane::Composer
            },
            ..Default::default()
        },
        &ModalKeys {
            history_searching: fixture == SurfaceFixture::HistorySearch,
            model_searching: matches!(
                fixture,
                SurfaceFixture::Models | SurfaceFixture::Connections
            ),
            ..Default::default()
        },
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    )
}

fn run_sheet_key(
    input: &mut String,
    cursor: &mut usize,
    code: KeyCode,
    modifiers: KeyModifiers,
    kind: crate::shell::sheet::SheetKind,
    has_focus: bool,
) -> InputAction {
    let mut drag = SelectionDrag::default();
    route_event(
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        input,
        cursor,
        Dispatch {
            overlay: None,
            sheet: Some(kind),
            focus: if has_focus {
                crate::shell::focus::FocusPlane::Transcript
            } else {
                crate::shell::focus::FocusPlane::Composer
            },
            ..Default::default()
        },
        &ModalKeys::default(),
        &SheetKeys {
            transcript_plane: has_focus,
            ..Default::default()
        },
        &SceneKeys::default(),
        &mut drag,
    )
}

fn run_history_key(
    input: &mut String,
    cursor: &mut usize,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> InputAction {
    let mut drag = SelectionDrag::default();
    route_event(
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        input,
        cursor,
        Dispatch {
            overlay: Some(crate::shell::surfaces::OverlaySurface::Dialog(
                crate::shell::surfaces::DialogKind::HistorySearch,
            )),
            ..Default::default()
        },
        &ModalKeys {
            history_searching: true,
            ..Default::default()
        },
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    )
}

fn editor_key(code: KeyCode, field: u8, input: &mut String) -> InputAction {
    let mut cursor = input.chars().count();
    let mut drag = SelectionDrag::default();
    route_event(
        Event::Key(graticule::event::KeyEvent::new(code, KeyModifiers::NONE)),
        input,
        &mut cursor,
        Dispatch {
            overlay: Some(crate::shell::surfaces::OverlaySurface::Sheet(
                crate::shell::surfaces::SheetKind::ModelEditor,
            )),
            ..Default::default()
        },
        &ModalKeys {
            editor_field: Some(field),
            ..Default::default()
        },
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    )
}

fn compose_key(
    code: KeyCode,
    modifiers: KeyModifiers,
    input: &mut String,
    cursor: &mut usize,
) -> InputAction {
    let mut drag = SelectionDrag::default();
    route_event(
        Event::Key(KeyEvent::new(code, modifiers)),
        input,
        cursor,
        Dispatch::default(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    )
}

fn pageup_key() -> InputAction {
    key_without_modal(KeyCode::PageUp)
}

fn key_without_modal(code: KeyCode) -> InputAction {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    route_event(
        Event::Key(graticule::event::KeyEvent::new(code, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        Dispatch::default(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    )
}

fn compose_key_with_completion(
    code: KeyCode,
    completion_kind: crate::shell::CompletionKind,
    suggestion_count: usize,
    exact: bool,
) -> InputAction {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    route_event(
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        Dispatch::default(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys {
            completion_kind,
            suggestion_count,
            has_exact_suggestion: exact,
            suggestion_index: Some(0),
            ..Default::default()
        },
        &mut drag,
    )
}

fn run_paste(
    text: &str,
    input: &mut String,
    cursor: &mut usize,
    fixture: SurfaceFixture,
) -> InputAction {
    let (overlay, scene) = fixture.to_dispatch();
    let mut drag = SelectionDrag::default();
    route_event(
        Event::Paste(text.to_string()),
        input,
        cursor,
        Dispatch {
            overlay,
            scene,
            ..Default::default()
        },
        &ModalKeys {
            history_searching: fixture == SurfaceFixture::HistorySearch,
            model_searching: matches!(
                fixture,
                SurfaceFixture::Models | SurfaceFixture::Connections
            ),
            ..Default::default()
        },
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    )
}

fn multiline_arrow(seed: &str, cursor: usize, code: KeyCode) -> (InputAction, usize) {
    let mut input = seed.to_string();
    let mut cur = cursor;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(graticule::event::KeyEvent::new(code, KeyModifiers::NONE)),
        &mut input,
        &mut cur,
        Dispatch::default(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    (action, cur)
}

#[test]
fn pre_attach_keys_including_space_toggle() {
    let dispatch = Dispatch {
        pre_attach: true,
        ..Default::default()
    };
    let mut input = String::new();
    let mut cur = 0;
    let mut drag = SelectionDrag::default();

    let mut route = |code: KeyCode| {
        route_event(
            Event::Key(KeyEvent::new(code, KeyModifiers::NONE)),
            &mut input,
            &mut cur,
            dispatch.clone(),
            &ModalKeys::default(),
            &SheetKeys::default(),
            &SceneKeys::default(),
            &mut drag,
        )
    };

    assert_eq!(route(KeyCode::Up), InputAction::PreAttachUp);
    assert_eq!(route(KeyCode::Down), InputAction::PreAttachDown);
    assert_eq!(route(KeyCode::Char(' ')), InputAction::PreAttachToggle);
    assert_eq!(route(KeyCode::Enter), InputAction::PreAttachSubmit);
    assert_eq!(route(KeyCode::Esc), InputAction::PreAttachCancel);
}

mod editing;
mod modals;
mod navigation;
mod paste_escape;
mod sheet_modal_arbitration;
mod submit;

#[test]
fn shift_arrows_select_messages_only_on_the_transcript_plane() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let transcript = route_event(
        Event::Key(graticule::event::KeyEvent::new(
            KeyCode::Down,
            KeyModifiers::SHIFT,
        )),
        &mut input,
        &mut cursor,
        Dispatch {
            focus: crate::shell::focus::FocusPlane::Transcript,
            ..Default::default()
        },
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(transcript, InputAction::SelectMessages { delta: 1 });

    let composer = route_event(
        Event::Key(graticule::event::KeyEvent::new(
            KeyCode::Down,
            KeyModifiers::SHIFT,
        )),
        &mut input,
        &mut cursor,
        Dispatch::default(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_ne!(composer, InputAction::SelectMessages { delta: 1 });
}

#[test]
fn graticule_vt_wire_bytes_route_end_to_end_through_nuo_keymap_and_router() {
    use graticule::{ParsedItem, VtParser};
    use std::time::{Duration, Instant};

    let mut parser = VtParser::new();
    let t0 = Instant::now();
    let mut items = Vec::new();

    // 1. Ctrl+Shift+P (both plain CSI-u and shifted-alternate CSI-u), Cmd+C, Shift+Tab.
    parser.feed(
        b"\x1b[112;6u\x1b[112:80;6u\x1b[99;9u\x1b[Z\x1b[9;2u",
        t0,
        &mut items,
    );
    let keys: Vec<_> = items
        .drain(..)
        .filter_map(|i| match i {
            ParsedItem::Event(Event::Key(k)) => Some(crate::shell::keymap::key_from_event(k)),
            _ => None,
        })
        .collect();
    assert_eq!(
        keys,
        vec![
            crate::shell::keymap::Key::CTRL_SHIFT_P,
            crate::shell::keymap::Key::CTRL_SHIFT_P,
            crate::shell::keymap::Key::CMD_C,
            crate::shell::keymap::Key::BACKTAB,
            crate::shell::keymap::Key::BACKTAB,
        ]
    );

    // 2. Split SGR hover across reads routes to Hover with zero leaked chars in composer.
    parser.feed(b"\x1b", t0, &mut items);
    assert!(items.is_empty());
    parser.feed(b"[<35;52;28M", t0 + Duration::from_millis(2), &mut items);
    let mut composer = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let actions: Vec<_> = items
        .drain(..)
        .filter_map(|i| match i {
            ParsedItem::Event(ev) => Some(route_event(
                ev,
                &mut composer,
                &mut cursor,
                Dispatch::default(),
                &ModalKeys::default(),
                &SheetKeys::default(),
                &SceneKeys::default(),
                &mut drag,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(actions, vec![InputAction::Hover { x: 51, y: 27 }]);
    assert!(
        composer.is_empty(),
        "split SGR must never inject composer text"
    );

    // 3. Alt+[ (Kitty), Alt+] (legacy), Alt-0, Alt-, route to exact global actions.
    parser.feed(b"\x1b[91;3u\x1b]\x1b0\x1b,", t0, &mut items);
    let nav_actions: Vec<_> = items
        .drain(..)
        .filter_map(|i| match i {
            ParsedItem::Event(ev) => Some(route_event(
                ev,
                &mut composer,
                &mut cursor,
                Dispatch::default(),
                &ModalKeys::default(),
                &SheetKeys::default(),
                &SceneKeys::default(),
                &mut drag,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        nav_actions,
        vec![
            InputAction::TabHistoryBack,
            InputAction::TabHistoryForward,
            InputAction::NavigateDashboard,
            InputAction::NavigateSettings,
        ]
    );
}
