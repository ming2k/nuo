//! Transient notice bubbles: copy result and armed-action toasts.

use nuotc::Frame;

use crate::shell::render::Theme;
use nuo_ui_primitives::components::toast::{ToastBubble, ToastKind};
use nuo_view::model::document::NoticeSeverity;

pub fn draw_armed_toast(frame: &mut Frame, message: &str, theme: &Theme) {
    ToastBubble {
        message,
        kind: ToastKind::Armed,
        hint: None,
    }
    .render(frame, theme);
}

pub fn draw_copy_toast(frame: &mut Frame, message: &str, failed: bool, theme: &Theme) {
    ToastBubble {
        message,
        kind: if failed {
            ToastKind::CopyFailed
        } else {
            ToastKind::CopyOk
        },
        hint: None,
    }
    .render(frame, theme);
}

/// Draw a toast-surfaced notice: a command acknowledgment such as
/// `/delegate on`, or the bubble that points at a fault just recorded in the
/// notice log.
///
/// The accent color follows the notice severity, reusing the same
/// severity→color map as the inline notice renderer so the two stay visually
/// consistent. `hint` is the pointer row ("Ctrl-o · all notices"): a bubble
/// that only says "something failed" and then disappears leaves the user with
/// nothing to act on, which is the defect this whole tier exists to remove.
pub fn draw_notice_toast(
    frame: &mut Frame,
    message: &str,
    severity: NoticeSeverity,
    theme: &Theme,
    hint: Option<&str>,
) {
    let kind = match severity {
        NoticeSeverity::Error => ToastKind::Error,
        NoticeSeverity::Warning => ToastKind::Warning,
        NoticeSeverity::Info => ToastKind::Info,
    };
    ToastBubble {
        message,
        kind,
        hint,
    }
    .render(frame, theme);
}
