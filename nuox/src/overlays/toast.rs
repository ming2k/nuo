//! Transient notice bubbles: copy result and armed-action toasts.

use nuotc::Frame;

use crate::components::toast::{ToastBubble, ToastKind};
use crate::model::document::NoticeSeverity;
use crate::render::Theme;

pub fn draw_armed_toast(frame: &mut Frame, message: &str, theme: &Theme) {
    ToastBubble {
        message,
        kind: ToastKind::Armed,
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
    }
    .render(frame, theme);
}

/// Draw a toast-surfaced notice (a command acknowledgment such as
/// `/delegate on`). The bubble's accent color follows the notice severity,
/// reusing the same severity→color map as the inline notice renderer so the
/// two stay visually consistent. Unlike the copy/armed toasts this is driven
/// by a `RoundEvent::Notice` forwarded across the listener→loop boundary.
pub fn draw_notice_toast(
    frame: &mut Frame,
    message: &str,
    severity: NoticeSeverity,
    theme: &Theme,
) {
    let kind = match severity {
        NoticeSeverity::Error => ToastKind::Error,
        NoticeSeverity::Warning => ToastKind::Warning,
        NoticeSeverity::Info => ToastKind::Info,
    };
    ToastBubble { message, kind }.render(frame, theme);
}
