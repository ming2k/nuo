//! Async clipboard plumbing for the event loop. Copies and pastes run in
//! spawned tasks so a stuck system clipboard (arboard / wl-copy / wl-paste)
//! can never freeze the TUI's event poll; results flow back through channels
//! and are applied on the next frame.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::mpsc;

use nuo_wire::ImagePart;

use crate::App;
use crate::clipboard::{self, ClipboardRead, CopyOutcome};
use crate::composer_attachments::{image_chip, paste_chip, paste_line_count, should_chip_paste};
use crate::surfaces::{DialogKind, OverlaySurface, SceneKind, SheetKind};

/// Which layer, if any, **declared** whether the active route takes images.
/// `None` = undeclared: the client must not gate on it (ADR-0230).
///
/// This is the paste gate's question, and it is deliberately not
/// [`App::active_model_supports_vision`]: that one answers the permissive
/// *request* policy ("would images travel?"), while a paste may only be
/// refused when a layer actually said the route rejects them. The authority is
/// still the daemon-built picker snapshot (ADR-0182) — the full ADR-0149
/// resolution for the route the daemon will serve — merely read as a
/// declaration rather than as a policy.
pub(crate) fn active_route_vision_declaration(app: &App) -> Option<bool> {
    app.active_route_capabilities().vision
}

/// The escape hatch named in the paste rejection: a declared text-only route is
/// the user's own `CapabilityOverrides::vision` decision away from accepting
/// images (ADR-0149 layer 1).
const VISION_OVERRIDE_HINT: &str = "set Vision in the model editor to override";

/// Suffix for a successful image paste on a route whose image support **no
/// layer declared**.
///
/// The paste is accepted — deliberately, since vendors rarely publish this
/// capability (ADR-0230) — but an accepted paste is not a promise that the
/// model will see the picture. Saying so up front costs one parenthetical and
/// is the difference between a user who understands the next failure and one
/// who thinks the client lost their image.
fn vision_unverified_suffix(app: &App) -> &'static str {
    if active_route_vision_declaration(app).is_none() {
        " (image support unverified for this model)"
    } else {
        ""
    }
}

/// Bound on each clipboard operation. A stuck reader must never freeze the
/// event loop's poll cadence.
const CLIP_TIMEOUT: Duration = Duration::from_secs(3);

pub(super) fn spawn_clipboard_copy(
    tx: &mpsc::UnboundedSender<Result<CopyOutcome, String>>,
    copy_pending: Arc<AtomicUsize>,
    text: String,
) {
    let tx = tx.clone();
    copy_pending.fetch_add(1, Ordering::SeqCst);
    tokio::spawn(async move {
        let result = match tokio::time::timeout(CLIP_TIMEOUT, clipboard::copy(&text)).await {
            Ok(inner) => inner,
            Err(_) => Err("clipboard copy timed out".to_string()),
        };
        let _ = tx.send(result);
        copy_pending.fetch_sub(1, Ordering::SeqCst);
    });
}

/// Read the system clipboard in a background task and deliver the result to
/// the event loop. Bounded by a timeout so a stuck clipboard reader can never
/// freeze paste feedback.
pub(super) fn spawn_clipboard_paste(tx: &mpsc::UnboundedSender<ClipboardRead>) {
    let tx = tx.clone();
    tokio::spawn(async move {
        let read = match tokio::time::timeout(CLIP_TIMEOUT, clipboard::read()).await {
            Ok(inner) => inner,
            Err(_) => ClipboardRead::Empty,
        };
        let _ = tx.send(read);
    });
}

/// Apply a completed clipboard paste: attach an image, insert text at the
/// cursor, or surface an error toast.
///
/// On the main prompt (`Modal::None`) a paste follows the chip-or-inline
/// composer semantics — images stage as `[Image #N]` attachments and large
/// text blocks collapse into `[Pasted text #N +M lines]` chips. Inside a
/// free-text modal (provider editor, picker filter, history
/// search) the input line is borrowed as a single-line field, so the paste
/// splices the text inline at the cursor with newlines stripped (matching
/// `insert_newline` being a no-op in modals) and skips the chip / attachment
/// machinery entirely. Other modals drop the paste silently.
pub(super) fn apply_clipboard_paste(app: &mut App, read: ClipboardRead) {
    // The question sheet's "Other" field accepts paste only while the sheet
    // is the keyboard foreground — a coexisting modal renders above it and
    // its paste lands in the modal's own borrowed field (or is dropped).
    if app.active_sheet() == Some(crate::sheet::SheetKind::Question)
        && app.active_dialog().is_none()
    {
        return apply_question_other_paste(app, read);
    }
    if let Some(overlay) = app.surfaces.active_overlay() {
        match overlay {
            OverlaySurface::Dialog(
                DialogKind::HistorySearch | DialogKind::Models | DialogKind::Connections,
            ) => {
                apply_modal_field_paste(app, read);
            }
            OverlaySurface::Sheet(SheetKind::ModelEditor | SheetKind::CustomProvider) => {
                apply_modal_field_paste(app, read);
            }
            _ => {}
        }
    } else if app.current_scene() == SceneKind::Settings {
        apply_modal_field_paste(app, read);
    } else {
        apply_composer_paste(app, read);
    }
}

/// Main-prompt paste: chips for images and large text blocks, inline insert
/// with a toast for short snippets. See [`apply_clipboard_paste`].
fn apply_composer_paste(app: &mut App, read: ClipboardRead) {
    match read {
        ClipboardRead::Image { data, mime } => {
            // Only a route that *declared* no image support refuses the paste,
            // and it says how to change that (ADR-0230). An undeclared route
            // attaches the image and lets the provider answer: whether a vendor
            // accepts images is exactly the kind of fact most of them do not
            // publish, so the client must not guess it on their behalf.
            if active_route_vision_declaration(app) == Some(false) {
                app.copy_toast_message = format!(
                    "{} does not support images — paste ignored ({VISION_OVERRIDE_HINT})",
                    app.current_model,
                );
                app.copy_toast_failed = true;
                app.copy_toast_until =
                    Some(std::time::Instant::now() + Duration::from_millis(2500));
                return;
            }
            let raw_size = data.len();
            let encoded = clipboard::base64_image(&data);
            app.pending_images.push(ImagePart {
                mime,
                data: encoded,
            });
            // Insert a short `[Image #N (size)]` chip at the cursor so the
            // user has a visible, atomic affordance for the staged
            // attachment — the chip is what they backspace to undo the
            // paste. The trailing space keeps the cursor on a word boundary
            // so typing resumes naturally. The size badge is the identifier's
            // payload info: the raw byte count of the image.
            let n = app.pending_images.len();
            insert_chip_at_cursor(app, &image_chip(n, raw_size));
            app.copy_toast_message = format!(
                "{n} image{} attached — enter to send{}",
                if n == 1 { "" } else { "s" },
                vision_unverified_suffix(app),
            );
            app.copy_toast_failed = false;
            app.copy_toast_until = Some(std::time::Instant::now() + Duration::from_millis(1800));
        }
        ClipboardRead::Files(paths) => {
            apply_composer_files_paste(app, paths);
        }
        ClipboardRead::Text(text) => {
            // A terminal paste (Ctrl+Shift+V — the terminal's own paste,
            // delivered as bracketed paste) is a text-only channel: an
            // image copied in a file manager arrives as the clipboard's
            // text flavor (the `file://` URI or bare path) while the
            // Ctrl+V clipboard read stages the image itself. Upgrade
            // payloads that are entirely references to existing local
            // files — at least one a supported image — to the same
            // attachment pipeline so both paste keys agree. Prose and
            // non-image paths stay verbatim below.
            if let Some(paths) = clipboard::paste_text_as_file_paths(&text)
                && paths.iter().any(|path| image_mime_for_path(path).is_some())
            {
                apply_composer_files_paste(app, paths);
                return;
            }
            // Large pastes (multi-line or long enough to balloon the input
            // box) are staged behind a `[Pasted text #N +M lines (size)]`
            // chip instead of being inlined verbatim. Short snippets keep
            // flowing through the cursor like an ordinary editor paste. The
            // line count and byte size in the label tell the user exactly
            // how much text the chip hides.
            if should_chip_paste(&text) {
                let n = app.pending_text_pastes.len() + 1;
                let line_count = paste_line_count(&text);
                let size_bytes = text.len();
                app.pending_text_pastes.push(text);
                insert_chip_at_cursor(app, &paste_chip(n, line_count, size_bytes));
                app.copy_toast_message = format!(
                    "pasted {line_count} line{} as a chip",
                    if line_count == 1 { "" } else { "s" }
                );
            } else {
                let chars_to_insert = text.chars().count();
                let byte_pos = app
                    .input
                    .char_indices()
                    .map(|(i, _)| i)
                    .nth(app.cursor_position)
                    .unwrap_or(app.input.len());
                app.input.insert_str(byte_pos, &text);
                app.set_cursor(app.cursor_position + chars_to_insert);
                app.copy_toast_message = format!(
                    "pasted {chars_to_insert} char{}",
                    if chars_to_insert == 1 { "" } else { "s" }
                );
            }
            app.copy_toast_failed = false;
            app.copy_toast_until = Some(std::time::Instant::now() + Duration::from_millis(1200));
        }
        ClipboardRead::Empty => {
            app.copy_toast_message = "clipboard is empty".to_string();
            app.copy_toast_failed = true;
            app.copy_toast_until = Some(std::time::Instant::now() + Duration::from_millis(1200));
        }
    }
}

/// Paste of file references (files copied in a file manager). Image files are
/// staged as `[Image #N]` attachments through the same pipeline as image-data
/// pastes; non-image files are skipped and reported in the toast rather than
/// silently dropped, and only a route that declared no image support blocks the
/// image (ADR-0230), with the same override hint as the image-data paste.
fn apply_composer_files_paste(app: &mut App, paths: Vec<std::path::PathBuf>) {
    let mut attached: Option<usize> = None;
    let mut skipped = 0usize;
    let mut vision_blocked = false;

    for path in paths {
        let Some((data, mime)) = read_image_file(&path) else {
            skipped += 1;
            continue;
        };
        if active_route_vision_declaration(app) == Some(false) {
            vision_blocked = true;
            break;
        }
        let raw_size = data.len();
        let encoded = clipboard::base64_image(&data);
        app.pending_images.push(ImagePart {
            mime,
            data: encoded,
        });
        let n = app.pending_images.len();
        insert_chip_at_cursor(app, &image_chip(n, raw_size));
        attached = Some(n);
    }

    app.copy_toast_message = match (attached, skipped, vision_blocked) {
        (Some(_), 0, false) => {
            let n = app.pending_images.len();
            format!(
                "{n} image{} attached — enter to send{}",
                if n == 1 { "" } else { "s" },
                vision_unverified_suffix(app),
            )
        }
        (Some(_), skipped, false) => {
            format!(
                "{} attached — skipped {skipped} non-image file{}",
                if app.pending_images.len() == 1 {
                    "1 image"
                } else {
                    "images"
                },
                if skipped == 1 { "" } else { "s" },
            )
        }
        (None, skipped, false) => {
            format!(
                "skipped {skipped} file{} — only images can be attached",
                if skipped == 1 { "" } else { "s" }
            )
        }
        (_, _, true) => format!(
            "{} does not support images — paste ignored ({VISION_OVERRIDE_HINT})",
            app.current_model,
        ),
    };
    app.copy_toast_failed = !matches!((attached, vision_blocked), (Some(_), false));
    app.copy_toast_until = Some(std::time::Instant::now() + Duration::from_millis(2000));
}

/// Extension→MIME map for paste-attachable raster images. Shared by the
/// file-reference paste and the text-payload sniff so both accept the same
/// format set (`ImagePart.mime` drives the provider payload; clipboard
/// pastes are always PNG but file copies can be any raster format).
fn image_mime_for_path(path: &std::path::Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// Read a pasted file's bytes if it is a supported image, keyed by extension.
fn read_image_file(path: &std::path::Path) -> Option<(Vec<u8>, String)> {
    let mime = image_mime_for_path(path)?.to_string();
    let data = std::fs::read(path).ok()?;
    (!data.is_empty()).then_some((data, mime))
}

/// Question-modal "Other" field paste. Unlike the readline modals, this field
/// owns its own buffer (`QuestionModel::other_text`) rather than borrowing
/// `App::input`, so it can't reuse [`apply_modal_field_paste`]. The text is
/// spliced through the model's pure `update` as a [`crate::question_model::QuestionAction::Paste`],
/// which appends it to the current question's "Other" field (newlines stripped
/// first). A no-op if the modal was closed or a real option is highlighted by
/// the time the (async) read lands. Images are rejected with a toast since the
/// field has no attachment staging. See [`apply_clipboard_paste`].
fn apply_question_other_paste(app: &mut App, read: ClipboardRead) {
    match read {
        ClipboardRead::Text(text) => {
            // Single-line: drop newlines the same way the modal-field path
            // does, since the "Other" field is a one-line text surface.
            let stripped: String = text.chars().filter(|&c| c != '\n' && c != '\r').collect();
            let chars = stripped.chars().count();
            if chars == 0 {
                return;
            }
            if let Some(qm) = app.question.take() {
                app.question = Some(
                    qm.update(crate::question_model::QuestionAction::Paste(stripped))
                        .0,
                );
                // A paste can span many lines once wrapped; re-arm follow so the
                // body scrolls to keep the caret (end of the pasted text) on
                // screen.
                app.question_modal_follow = true;
            }
            app.copy_toast_message =
                format!("pasted {chars} char{}", if chars == 1 { "" } else { "s" });
            app.copy_toast_failed = false;
            app.copy_toast_until = Some(std::time::Instant::now() + Duration::from_millis(1200));
        }
        ClipboardRead::Image { .. } => {
            app.copy_toast_message = "can't paste image into this field".to_string();
            app.copy_toast_failed = true;
            app.copy_toast_until = Some(std::time::Instant::now() + Duration::from_millis(1200));
        }
        ClipboardRead::Files(..) => {
            app.copy_toast_message = "can't paste files into this field".to_string();
            app.copy_toast_failed = true;
            app.copy_toast_until = Some(std::time::Instant::now() + Duration::from_millis(1200));
        }
        ClipboardRead::Empty => {
            app.copy_toast_message = "clipboard is empty".to_string();
            app.copy_toast_failed = true;
            app.copy_toast_until = Some(std::time::Instant::now() + Duration::from_millis(1200));
        }
    }
}
/// to preserve single-line semantics (the provider editor's API-key and
/// model-id fields, the picker filter, and the history search query are all
/// single-line). Image and file pastes are dropped with a short toast since
/// the modal field has no attachment staging. See [`apply_clipboard_paste`].
fn apply_modal_field_paste(app: &mut App, read: ClipboardRead) {
    match read {
        ClipboardRead::Text(text) => {
            // Collapse any newlines (and trailing carriage returns) so a
            // copied multi-line block pastes as one continuous line, matching
            // the single-line editing the modal fields already enforce.
            let stripped: String = text.chars().filter(|&c| c != '\n' && c != '\r').collect();
            let chars_to_insert = stripped.chars().count();
            if chars_to_insert == 0 {
                return;
            }
            let byte_pos = app
                .input
                .char_indices()
                .map(|(i, _)| i)
                .nth(app.cursor_position)
                .unwrap_or(app.input.len());
            app.input.insert_str(byte_pos, &stripped);
            app.set_cursor(app.cursor_position + chars_to_insert);
            app.copy_toast_message = format!(
                "pasted {chars_to_insert} char{}",
                if chars_to_insert == 1 { "" } else { "s" }
            );
            app.copy_toast_failed = false;
            app.copy_toast_until = Some(std::time::Instant::now() + Duration::from_millis(1200));
        }
        ClipboardRead::Image { .. } => {
            // Modal fields are single-line text; images are not attachable
            // here. Surface a brief toast so the paste is not silently lost.
            app.copy_toast_message = "can't paste image into this field".to_string();
            app.copy_toast_failed = true;
            app.copy_toast_until = Some(std::time::Instant::now() + Duration::from_millis(1200));
        }
        ClipboardRead::Files(..) => {
            app.copy_toast_message = "can't paste files into this field".to_string();
            app.copy_toast_failed = true;
            app.copy_toast_until = Some(std::time::Instant::now() + Duration::from_millis(1200));
        }
        ClipboardRead::Empty => {
            app.copy_toast_message = "clipboard is empty".to_string();
            app.copy_toast_failed = true;
            app.copy_toast_until = Some(std::time::Instant::now() + Duration::from_millis(1200));
        }
    }
}

/// Splice `chip` followed by a single space into [`App::input`] at the
/// cursor, advancing the cursor past both. Shared by the image and large-
/// text paste paths so the chip's surrounding whitespace stays consistent —
/// the trailing space is what lets the chip-aware Backspace erase the whole
/// paste in one keystroke.
fn insert_chip_at_cursor(app: &mut App, chip: &str) {
    let byte_pos = app
        .input
        .char_indices()
        .map(|(i, _)| i)
        .nth(app.cursor_position)
        .unwrap_or(app.input.len());
    let mut spliced = String::with_capacity(chip.len() + 1);
    spliced.push_str(chip);
    spliced.push(' ');
    let extra_chars = spliced.chars().count();
    app.input.insert_str(byte_pos, &spliced);
    app.set_cursor(app.cursor_position + extra_chars);
}

pub(super) fn set_copy_feedback(app: &mut App, result: Result<CopyOutcome, String>) {
    match result {
        Ok(CopyOutcome::Native) => {
            app.copy_toast_message = "copied to clipboard".to_string();
            app.copy_toast_failed = false;
        }
        Ok(CopyOutcome::Osc52) => {
            app.copy_toast_message = "copy sent via OSC52".to_string();
            app.copy_toast_failed = false;
        }
        Err(error) => {
            let mut chars = error.chars();
            let prefix = chars.by_ref().take(48).collect::<String>();
            app.copy_toast_message = if chars.next().is_some() {
                format!("copy failed: {}...", prefix)
            } else {
                format!("copy failed: {}", prefix)
            };
            app.copy_toast_failed = true;
        }
    }
}
