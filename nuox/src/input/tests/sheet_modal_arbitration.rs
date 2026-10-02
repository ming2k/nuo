//! The coexisting-modal arbitration tests (ADR-0173 §3, revised contract):
//! when an overlay modal is open above a mounted interaction sheet, the
//! modal is the visual foreground and owns every non-global key. The
//! regression these lock: pressing Esc while a modal floats over the
//! permission sheet used to punch through and REJECT the pending permission
//! outright — the most destructive possible misroute.

use super::*;
use crate::sheet::SheetKind;

/// Build a Dispatch with a modal and a sheet mounted at once — the
/// "opened a picker while a permission is pending" state.
fn overlaid(fixture: SurfaceFixture, sheet: SheetKind) -> Dispatch {
    let (overlay, scene) = fixture.to_dispatch();
    Dispatch {
        overlay,
        scene,
        sheet: Some(sheet),
        ..Default::default()
    }
}

fn key(code: KeyCode) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent::new(code, KeyModifiers::NONE)
}

fn route(dispatch: Dispatch, code: KeyCode) -> InputAction {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    route_event(
        Event::Key(key(code)),
        &mut input,
        &mut cursor,
        dispatch,
        &ModalKeys::default(),
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    )
}

#[test]
fn esc_over_modal_never_rejects_the_permission_beneath() {
    for fixture in [
        SurfaceFixture::UsageStats,
        SurfaceFixture::Models,
        SurfaceFixture::Tools,
        SurfaceFixture::Queue,
        SurfaceFixture::Telemetry,
        SurfaceFixture::Sessions,
    ] {
        let action = route(overlaid(fixture, SheetKind::Permission), KeyCode::Esc);
        assert_ne!(
            action,
            InputAction::PermissionReject,
            "Esc over {fixture:?} must not punch through to the permission sheet"
        );
        assert_ne!(
            action,
            InputAction::PermissionSubmit,
            "Esc over {fixture:?} must not submit the permission sheet either"
        );
    }
    // The canonical case: a plain dismissable browse modal — Esc closes it.
    assert_eq!(
        route(
            overlaid(SurfaceFixture::UsageStats, SheetKind::Permission),
            KeyCode::Esc
        ),
        InputAction::CloseModal
    );
}

#[test]
fn esc_rejects_permission_only_while_it_is_the_foreground() {
    // No modal coexisting: Esc is the sheet's own reject gesture (the
    // original contract, unchanged).
    assert_eq!(
        route(
            overlaid(SurfaceFixture::None, SheetKind::Permission),
            KeyCode::Esc
        ),
        InputAction::PermissionReject
    );
}

#[test]
fn enter_over_modal_never_submits_the_permission_beneath() {
    // Enter resolves the modal's own verb (Usage stats closes); it must never
    // reach the sheet's PermissionSubmit — a stray Enter while browsing
    // the Usage stats view would otherwise grant the tool call.
    assert_eq!(
        route(
            overlaid(SurfaceFixture::UsageStats, SheetKind::Permission),
            KeyCode::Enter
        ),
        InputAction::CloseModal
    );
    // Foreground sheet: Enter submits, as before.
    assert_eq!(
        route(
            overlaid(SurfaceFixture::None, SheetKind::Permission),
            KeyCode::Enter
        ),
        InputAction::PermissionSubmit
    );
}

#[test]
fn arrows_navigate_the_modal_not_the_sheet_beneath() {
    // ↑/↓ over a coexisting modal walk the modal's list (ModalUp/ModalDown),
    // never the sheet's decision cursor or the transcript behind it.
    assert_eq!(
        route(
            overlaid(SurfaceFixture::Tools, SheetKind::Permission),
            KeyCode::Up
        ),
        InputAction::SessionSelect { forward: false }
    );
    assert_eq!(
        route(
            overlaid(SurfaceFixture::Tools, SheetKind::Permission),
            KeyCode::Down
        ),
        InputAction::SessionSelect { forward: true }
    );
    // Foreground sheet: ↑ scrolls the transcript (the pass-through claim).
    assert_eq!(
        route(
            overlaid(SurfaceFixture::None, SheetKind::Permission),
            KeyCode::Up
        ),
        InputAction::ScrollUp
    );
}

#[test]
fn sheet_verbs_suspend_while_a_modal_is_open() {
    // The permission sheet owns ←/→/Tab for its decision cursor — but only
    // as the foreground. Over a modal those keys are inert to the sheet
    // (they fall through to the shared layers, which no-op them here).
    for code in [KeyCode::Left, KeyCode::Right, KeyCode::Tab] {
        assert_eq!(
            route(
                overlaid(SurfaceFixture::UsageStats, SheetKind::Permission),
                code
            ),
            InputAction::None,
            "sheet verb {code:?} must not fire through a coexisting modal"
        );
    }
    // Foreground sheet: the verbs fire.
    assert_eq!(
        route(
            overlaid(SurfaceFixture::None, SheetKind::Permission),
            KeyCode::Tab
        ),
        InputAction::PermissionNextOption
    );
}

#[test]
fn question_sheet_esc_and_printables_suspend_over_a_modal() {
    // Esc over a modal must not cancel the question beneath.
    assert_eq!(
        route(
            overlaid(SurfaceFixture::UsageStats, SheetKind::Question),
            KeyCode::Esc
        ),
        InputAction::CloseModal
    );
    assert_eq!(
        route(
            overlaid(SurfaceFixture::None, SheetKind::Question),
            KeyCode::Esc
        ),
        InputAction::QuestionCancel
    );
    // Printable verbs (space toggles an option) also suspend: the shared
    // char arm is inert with no editable field.
    assert_eq!(
        route(
            overlaid(SurfaceFixture::UsageStats, SheetKind::Question),
            KeyCode::Char(' ')
        ),
        InputAction::None
    );
}

#[test]
fn modal_verbs_still_reach_the_modal_over_a_sheet() {
    // The sheet must not swallow the modal's own verb keys either: the MCP
    // manager's `r` reconnect fires normally above a pending permission.
    assert_eq!(
        route(
            overlaid(SurfaceFixture::Mcp, SheetKind::Permission),
            KeyCode::Char('r')
        ),
        InputAction::McpReconnect
    );
}

#[test]
fn injection_sheet_input_suspends_over_a_text_modal() {
    // The injection sheet borrows the composer line — but over the
    // ModelEditor (which borrows it for its API-key field) printable keys
    // edit the modal's field, not double-land on the sheet's draft.
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(key(KeyCode::Char('x'))),
        &mut input,
        &mut cursor,
        overlaid(SurfaceFixture::ModelEditor, SheetKind::InputInjection),
        &ModalKeys {
            editor_field: Some(0),
            ..Default::default()
        },
        &SheetKeys::default(),
        &SceneKeys::default(),
        &mut drag,
    );
    assert_eq!(action, InputAction::InsertChar('x'));
    assert_eq!(input, "x", "the key edited the modal's borrowed field");
}
