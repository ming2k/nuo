//! The input test suite, split by interaction concern. Shared key-event
//! builders live here; per-concern groups are sibling modules.

use super::*;
use crate::modal_keys::ModalKeys;
use crate::session::SceneKeys;
use crate::sheet::SheetKeys;
use crossterm::event::{KeyEvent, KeyEventKind, KeyEventState};

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
    kind: crate::CompletionKind,
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
        // Surface dispatch keys off the explicit scene (ADR-0205), not the
        // legacy flags.
        dispatch.scene = if in_subagent_view {
            crate::surfaces::SceneKind::TaskInspection
        } else {
            crate::surfaces::SceneKind::Conversation
        };
    })
}

fn key_in_side_view_with(
    code: KeyCode,
    input: &mut String,
    tune: impl FnOnce(&mut Dispatch),
) -> InputAction {
    let mut cursor = input.chars().count();
    let mut drag = SelectionDrag::default();
    let mut dispatch = Dispatch {
        scene: crate::surfaces::SceneKind::Aside,
        ..Default::default()
    };
    tune(&mut dispatch);
    route_event(
        Event::Key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE)),
        input,
        &mut cursor,
        dispatch,
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    )
}

fn key_in_side_view(code: KeyCode, input: &mut String) -> InputAction {
    key_in_side_view_with(code, input, |_| {})
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
        Event::Key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE)),
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
        Event::Key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        Dispatch {
            focused_target: true,
            ..Default::default()
        },
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys {
            focused_target: true,
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
    Telemetry,
    Sessions,
    Queue,
    HistorySearch,
    Models,
    Connections,
    Skills,
    Tools,
    Mcp,
    Btw,
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
        Option<crate::surfaces::OverlaySurface>,
        crate::surfaces::SceneKind,
    ) {
        use crate::surfaces::{DialogKind, OverlaySurface, SceneKind, SheetKind};
        match self {
            Self::None => (None, SceneKind::Conversation),
            Self::Config => (None, SceneKind::Settings),
            Self::UsageStats => (
                Some(OverlaySurface::Dialog(DialogKind::UsageStats)),
                SceneKind::Conversation,
            ),
            Self::Telemetry => (
                Some(OverlaySurface::Dialog(DialogKind::Telemetry)),
                SceneKind::Conversation,
            ),
            Self::Sessions => (
                Some(OverlaySurface::Dialog(DialogKind::Sessions)),
                SceneKind::Conversation,
            ),
            Self::Queue => (
                Some(OverlaySurface::Dialog(DialogKind::Queue)),
                SceneKind::Conversation,
            ),
            Self::HistorySearch => (
                Some(OverlaySurface::Dialog(DialogKind::HistorySearch)),
                SceneKind::Conversation,
            ),
            Self::Models => (
                Some(OverlaySurface::Dialog(DialogKind::Models)),
                SceneKind::Conversation,
            ),
            Self::Connections => (
                Some(OverlaySurface::Dialog(DialogKind::Connections)),
                SceneKind::Conversation,
            ),
            Self::Skills => (
                Some(OverlaySurface::Dialog(DialogKind::Skills)),
                SceneKind::Conversation,
            ),
            Self::Tools => (
                Some(OverlaySurface::Dialog(DialogKind::Tools)),
                SceneKind::Conversation,
            ),
            Self::Mcp => (
                Some(OverlaySurface::Dialog(DialogKind::Mcp)),
                SceneKind::Conversation,
            ),
            Self::Btw => (
                Some(OverlaySurface::Dialog(DialogKind::Asides)),
                SceneKind::Conversation,
            ),
            Self::Permissions => (
                Some(OverlaySurface::Dialog(DialogKind::Permissions)),
                SceneKind::Conversation,
            ),
            Self::ProviderPreset => (
                Some(OverlaySurface::Sheet(SheetKind::ProviderPreset)),
                SceneKind::Conversation,
            ),
            Self::CustomProvider => (
                Some(OverlaySurface::Sheet(SheetKind::CustomProvider)),
                SceneKind::Conversation,
            ),
            Self::OauthPending => (
                Some(OverlaySurface::Sheet(SheetKind::OAuthPending)),
                SceneKind::Conversation,
            ),
            Self::ModelEditor => (
                Some(OverlaySurface::Sheet(SheetKind::ModelEditor)),
                SceneKind::Conversation,
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
            focused_target: has_focus,
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
    kind: crate::sheet::SheetKind,
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
            focused_target: has_focus,
            ..Default::default()
        },
        &ModalKeys::default(),
        &SheetKeys {
            focused_target: has_focus,
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
            overlay: Some(crate::surfaces::OverlaySurface::Dialog(
                crate::surfaces::DialogKind::HistorySearch,
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
        Event::Key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE)),
        input,
        &mut cursor,
        Dispatch {
            overlay: Some(crate::surfaces::OverlaySurface::Sheet(
                crate::surfaces::SheetKind::ModelEditor,
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
        Event::Key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE)),
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
    completion_kind: crate::CompletionKind,
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
        Event::Key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE)),
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

fn leaked_char(c: char) -> Event {
    Event::Key(KeyEvent {
        code: KeyCode::Char(c),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

fn leaked_esc() -> Event {
    Event::Key(KeyEvent {
        code: KeyCode::Esc,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

fn drain_guard(events: &[Event]) -> (usize, usize) {
    let mut g = SgrLeakGuard::default();
    let mut dropped = 0;
    let mut accepted = 0;
    for ev in events {
        match g.feed(ev) {
            Feed::Drop => dropped += 1,
            Feed::Accept => accepted += 1,
        }
    }
    (accepted, dropped)
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
