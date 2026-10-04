//! AI-initiated interaction sheets (ADR-0173 §3): the inline surfaces that
//! replace the composer slot when the agent needs a decision from the user.
//!
//! A sheet is *not* a modal and *not* a surface: it never floats above the
//! session view, so it has no `Recess` policy, no outside-click dismissal,
//! and no centered geometry. It is a **sibling component of the composer**:
//! the view's bottom slot mounts either the draft editor or one sheet, the
//! sheet replacing the composer wholesale (same bottom edge, height computed
//! from its content), while the transcript behind it stays live and
//! scrollable. Slot state lives on `App::active_sheet` — the surface router
//! never sees it. The initiator taxonomy is the boundary: AI → user
//! interaction requests are sheets; user-invoked tools are modals;
//! user-invoked spaces are full-screen views.

/// Keyboard claims for action surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Claims {
    pub text_entry: bool,
    pub list_nav: bool,
    pub body_scroll: bool,
    pub decide: bool,
    pub opaque: bool,
    pub composer_line: bool,
    pub transcript_nav: bool,
}

/// The sheet handlers' own sub-state (ADR-0197 M2): which sheet row/sub-view
/// is live and whether the transcript navigation facts pass through. Built
/// once per event by the caller; every read below is sheet-local.
#[derive(Debug, Default, Clone)]
pub struct SheetKeys {
    /// Whether the Question sheet's synthetic "Other" free-text row is the
    /// highlighted row. Only meaningful while the foreground sheet is
    /// `SheetKind::Question`: when `true` the sheet owns a text-input
    /// surface, so printable keys (including Space) insert into the "Other"
    /// field instead of toggling an option. Mirrors
    /// `App::question.is_some_and(|q| q.is_other_highlighted())`.
    pub question_other_highlighted: bool,
    /// Whether the permission sheet's decision cursor sits on "Always allow".
    pub permission_confirm_always: bool,
    /// Whether the inline permission sheet is expanded to "Details". Drives
    /// whether ↑/↓ in the compose zone scroll the details body or the
    /// transcript behind it.
    pub permission_show_details: bool,
    /// Whether a transcript step/action target holds keyboard focus behind
    /// the sheet (the permission sheet is the one pass-through surface).
    pub focused_target: bool,
}

/// The AI-initiated interaction sheets, in queue arrival order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SheetKind {
    /// Tool-permission approval sheet.
    Permission,
    /// `ask_user` question sheet.
    Question,
    /// Interactive-input injection sheet (L3.5 β).
    InputInjection,
}

pub(crate) use SheetKind::{InputInjection, Permission, Question};

impl SheetKind {
    /// Every sheet kind, for exhaustive policy assertions.
    pub const ALL: [SheetKind; 3] = [Permission, Question, InputInjection];

    /// The keyboard-ownership declaration for this sheet (ADR-0173 §2).
    ///
    /// The permission sheet is the one deliberate pass-through surface: it
    /// owns only its decision keys, and transcript navigation/scrolling fall
    /// through to the chat surface beneath so the history stays readable
    /// while a prompt is pending. The other sheets consume the keyboard
    /// (unhandled keys are inert, never passed down).
    pub fn keyboard_claims(self) -> Claims {
        match self {
            Permission => Claims {
                text_entry: false,
                list_nav: false,
                body_scroll: false,
                decide: true,
                opaque: false,
                composer_line: false,
                transcript_nav: true,
            },
            Question => Claims {
                text_entry: false,
                list_nav: true,
                body_scroll: true,
                decide: true,
                opaque: true,
                composer_line: false,
                transcript_nav: false,
            },
            // The injection sheet borrows the composer line itself: full
            // text entry, no scrollable body.
            InputInjection => Claims {
                text_entry: true,
                list_nav: false,
                body_scroll: false,
                decide: true,
                opaque: true,
                composer_line: true,
                transcript_nav: false,
            },
        }
    }
}

/// The sheets' self-owned key schemes (ADR-0173 §3): a key the sheet owns
/// resolves here; everything else falls through to the shared affordance
/// library and the sheet arms of the input router.
///
/// The question sheet's printable family: `space` toggles the highlighted
/// option, digits pick an option outright, every other character edits the
/// "Other" free-text field. The permission and input-injection sheets are
/// keyboard-driven through Enter/Esc/←/→ (decision cursor) and own no
/// printable verbs.
/// Whether `kind` (the foreground sheet) owns a bracketed-paste text field:
/// the Question sheet's "Other" free-text row, when it is highlighted. The
/// field owns its own buffer, so the paste payload routes there instead of
/// through the shared composer-line path.
pub fn sheet_owns_bracketed_paste(kind: SheetKind, keys: &SheetKeys) -> bool {
    matches!(kind, SheetKind::Question) && keys.question_other_highlighted
}

pub(crate) fn resolve_sheet_key(
    kind: SheetKind,
    key: crate::keymap::Key,
    keys: &SheetKeys,
) -> Option<crate::input::InputAction> {
    use crate::input::InputAction;
    use crossterm::event::KeyCode;

    let ctrl_alt_super = key.modifiers.intersects(
        crossterm::event::KeyModifiers::CONTROL
            | crossterm::event::KeyModifiers::ALT
            | crossterm::event::KeyModifiers::SUPER,
    );

    match kind {
        SheetKind::Permission => match key.code {
            KeyCode::Left if !ctrl_alt_super => Some(InputAction::PermissionPrevOption),
            KeyCode::Right if !ctrl_alt_super => Some(InputAction::PermissionNextOption),
            KeyCode::Tab if !ctrl_alt_super => Some(InputAction::PermissionNextOption),
            KeyCode::BackTab => Some(InputAction::PermissionPrevOption),
            // Decision + navigation family moved from the router's fallback
            // arms (ADR-0173 §3): Enter commits the pending decision (Alt+
            // Enter stays the shared multi-line newline chord), and ↑/↓ act
            // on the focused step first, then the details body, then fall
            // back to transcript scrolling — the permission sheet is the one
            // pass-through surface.
            KeyCode::Enter if !key.modifiers.contains(crossterm::event::KeyModifiers::ALT) => {
                Some(InputAction::PermissionSubmit)
            }
            KeyCode::Up => {
                if keys.focused_target {
                    Some(InputAction::FocusPrevTarget)
                } else if keys.permission_show_details {
                    Some(InputAction::PermissionDetailsUp)
                } else {
                    Some(InputAction::ScrollUp)
                }
            }
            KeyCode::Down => {
                if keys.focused_target {
                    Some(InputAction::FocusNextTarget)
                } else if keys.permission_show_details {
                    Some(InputAction::PermissionDetailsDown)
                } else {
                    Some(InputAction::ScrollDown)
                }
            }
            _ => None,
        },
        SheetKind::Question => match key.code {
            KeyCode::Tab if !ctrl_alt_super => Some(InputAction::QuestionNext),
            KeyCode::BackTab => Some(InputAction::QuestionPrevious),
            KeyCode::Left if !ctrl_alt_super && !keys.question_other_highlighted => {
                Some(InputAction::QuestionPrevious)
            }
            KeyCode::Right if !ctrl_alt_super && !keys.question_other_highlighted => {
                Some(InputAction::QuestionNext)
            }
            KeyCode::Char(c) if !ctrl_alt_super => {
                if c == ' ' && !keys.question_other_highlighted {
                    Some(InputAction::QuestionToggle)
                } else if let Some(d) = c.to_digit(10)
                    && (1..=9).contains(&d)
                {
                    Some(InputAction::QuestionSelect(d as usize))
                } else {
                    Some(InputAction::QuestionInsertChar(c))
                }
            }
            // Decision + navigation family moved from the router's fallback
            // arms (ADR-0173 §3): Enter commits, ↑/↓ walk the option cursor,
            // Backspace edits the "Other" field, and Ctrl+V routes the paste
            // into that field's own buffer.
            KeyCode::Enter if !key.modifiers.contains(crossterm::event::KeyModifiers::ALT) => {
                Some(InputAction::QuestionSubmit)
            }
            KeyCode::Up => Some(InputAction::QuestionUp),
            KeyCode::Down => Some(InputAction::QuestionDown),
            KeyCode::Backspace => Some(InputAction::QuestionBackspace),
            KeyCode::Char('v')
                if key
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL)
                    && keys.question_other_highlighted =>
            {
                // The "Other" field owns its own buffer (not `App::input`),
                // so it can't share `edits_input_field` with the readline
                // modals. Route through the same async paste path; the event
                // loop applies the read to `QuestionModel::other_text`.
                Some(InputAction::Paste)
            }
            _ => None,
        },
        SheetKind::InputInjection => {
            // Enter submits the injected input (Alt+Enter stays the shared
            // newline chord); ↑/↓ are inert — the injection sheet borrows
            // the composer line but has no option list.
            match key.code {
                KeyCode::Enter if !key.modifiers.contains(crossterm::event::KeyModifiers::ALT) => {
                    Some(InputAction::InputSubmit)
                }
                _ => None,
            }
        }
    }
}
