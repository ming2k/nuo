//! Workspace effect reversal: writing recorded pre-images back, and refusing to
//! when the file has moved on (ADR-0125).
//!
//! ## Reversal is a three-way check, not a write
//!
//! `/undo` is not "restore the snapshot". Between the moment a thread wrote a
//! file and the moment the user runs `/undo`, three things may have happened: the
//! user edited it, a later round edited it, or another process (a formatter, a
//! build, an editor) edited it. None of those are this thread's work, and
//! overwriting them would destroy data in the name of protecting it.
//!
//! So every reversal compares the file's *current* content against the
//! post-image the journal recorded:
//!
//! ```text
//!   current == after_hash   →  nobody changed it since; restore before_hash
//!   current != after_hash   →  the file moved on; refuse, and say so
//!   file missing, op==create →  already removed; nothing to do
//!   file missing, op==edit   →  deleted out from under us; refuse
//! ```
//!
//! A refusal is the correct outcome, not a failure. It is what lets `/undo` be
//! trusted: the user knows that when it says "restored", the bytes are back, and
//! when it says "not restored", nothing was silently clobbered.
//!
//! ## Ordering
//!
//! Effects are reversed **newest first**. Two writes to one path in one round
//! unwind through their own history: the second write's post-image is what is on
//! disk now, so it is restored to the first write's post-image, which is then
//! restored to the original pre-image. Reversing oldest-first would try to write
//! the original pre-image over content whose post-image does not match, and
//! refuse a file that was perfectly undoable.
//!
//! ## What this module does not do
//!
//! It does not decide *which* rows to undo (that is the round-scoped journal
//! query), whether a row is addressable at all (that is the row's own
//! `is_undoable`), or what the user is told (that is
//! [`CommandResult::UndoOutcome`](nuo_wire::CommandResult::UndoOutcome)). It takes
//! candidate rows and produces the outcome of attempting each one.
//!
//! ## Content residency
//!
//! Pre-images are read back through [`PreImageStore`] and written with an atomic
//! replace, so a crash mid-restore leaves either the old or the new bytes and
//! never a truncated file. Byte-for-byte restoration is what makes the journal's
//! promise meaningful: "reversible" means the file is *identical*, not that it
//! resembles its earlier self.

use std::path::{Path, PathBuf};

use nuo_persistence::workspace_effects::{JournalOp, WorkspaceEffect, WorkspaceEffectReversal};
use nuo_wire::{ChangedFile, UndoRefusal};

use crate::runtime::workspace_journal::PreImageStore;

/// The outcome of attempting to reverse one row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reversal {
    /// The pre-image was written back.
    Restored(ChangedFile),
    /// The file moved on; nothing was written.
    Refused(UndoRefusal),
    /// The row was not addressable in the first place (not reversible, not
    /// completed, or already reversed). Not an error: the caller asked about a
    /// row that a concurrent `/undo` may have handled.
    Skipped,
}

/// Reverse one journaled effect.
///
/// `workspace_root` is used to render the path the way the user sees it;
/// `store` supplies the pre-image bytes. `now_ms` is stamped on the row by the
/// caller, not here, because this function performs no persistence.
pub fn reverse(
    effect: &WorkspaceEffect,
    workspace_root: Option<&Path>,
    store: &dyn PreImageStore,
) -> Reversal {
    let display = display_path(effect.path.as_deref(), workspace_root);

    if !effect.is_undoable() {
        return Reversal::Skipped;
    }
    let Some(path) = effect.path.as_deref() else {
        // A reversible row always has a path; one without is a corrupted row,
        // and refusing is the only safe reading.
        return Reversal::Refused(UndoRefusal {
            path: display,
            reason: "the journal row records no path".to_string(),
        });
    };
    let path = PathBuf::from(path);

    // Compare the file's current content against the recorded post-image.
    let current = match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Reversal::Refused(UndoRefusal {
                path: display,
                reason: format!("cannot read the file to check it: {error}"),
            });
        }
    };

    match effect.op {
        JournalOp::Create => match current {
            // The file this call created is still exactly as it was created:
            // undo means remove it.
            Some(bytes) if store.put(&bytes).ok().as_deref() == effect.after_hash.as_deref() => {
                if let Err(error) = std::fs::remove_file(&path) {
                    return Reversal::Refused(UndoRefusal {
                        path: display,
                        reason: format!("cannot remove the created file: {error}"),
                    });
                }
                Reversal::Restored(changed_file(effect, display, &path, true, false))
            }
            // Already gone: the desired end state.
            None => Reversal::Restored(changed_file(effect, display, &path, true, false)),
            // Someone wrote to it after this thread created it.
            Some(_) => Reversal::Refused(UndoRefusal {
                path: display,
                reason: "the file changed after this thread created it; \
                         removing it would discard that work"
                    .to_string(),
            }),
        },
        JournalOp::Edit => {
            let Some(current) = current else {
                return Reversal::Refused(UndoRefusal {
                    path: display,
                    reason: "the file was deleted after this thread wrote it; \
                             restoring it would recreate something the user removed"
                        .to_string(),
                });
            };
            let current_hash = match store.put(&current) {
                Ok(hash) => hash,
                Err(error) => {
                    return Reversal::Refused(UndoRefusal {
                        path: display,
                        reason: format!("cannot hash the current content: {error}"),
                    });
                }
            };
            if Some(current_hash.as_str()) != effect.after_hash.as_deref() {
                return Reversal::Refused(UndoRefusal {
                    path: display,
                    reason: "the file changed after this thread wrote it; \
                             restoring the old content would discard that work"
                        .to_string(),
                });
            }
            let Some(before_hash) = effect.before_hash.as_deref() else {
                return Reversal::Refused(UndoRefusal {
                    path: display,
                    reason: "the journal row records no pre-image to restore".to_string(),
                });
            };
            let Some(bytes) = store.get(before_hash) else {
                return Reversal::Refused(UndoRefusal {
                    path: display,
                    reason: "the recorded pre-image is no longer available".to_string(),
                });
            };
            if let Err(error) = nuo_host::fsutil::atomic_write_bytes(&path, &bytes) {
                return Reversal::Refused(UndoRefusal {
                    path: display,
                    reason: format!("cannot write the pre-image back: {error}"),
                });
            }
            Reversal::Restored(changed_file(effect, display, &path, false, false))
        }
        JournalOp::Delete => {
            // No built-in tool produces a delete today; the variant exists so a
            // future one cannot be recorded as an edit with empty content.
            // Restoring a deletion means recreating the file from the pre-image.
            let Some(before_hash) = effect.before_hash.as_deref() else {
                return Reversal::Refused(UndoRefusal {
                    path: display,
                    reason: "the journal row records no pre-image to recreate".to_string(),
                });
            };
            if current.is_some() {
                return Reversal::Refused(UndoRefusal {
                    path: display,
                    reason: "a file now exists where this thread deleted one; \
                             restoring would overwrite it"
                        .to_string(),
                });
            }
            let Some(bytes) = store.get(before_hash) else {
                return Reversal::Refused(UndoRefusal {
                    path: display,
                    reason: "the recorded pre-image is no longer available".to_string(),
                });
            };
            if let Some(parent) = path.parent()
                && let Err(error) = std::fs::create_dir_all(parent)
            {
                return Reversal::Refused(UndoRefusal {
                    path: display,
                    reason: format!("cannot recreate the parent directory: {error}"),
                });
            }
            if let Err(error) = nuo_host::fsutil::atomic_write_bytes(&path, &bytes) {
                return Reversal::Refused(UndoRefusal {
                    path: display,
                    reason: format!("cannot recreate the file: {error}"),
                });
            }
            Reversal::Restored(changed_file(effect, display, &path, false, true))
        }
    }
}

/// Reverse a whole set of rows, newest first, producing the pairs the caller
/// must persist.
///
/// The returned [`WorkspaceEffectReversal`] values are what the caller stamps
/// onto the journal rows; this module never writes to the journal, so the
/// "restored" bookkeeping and the bytes on disk stay one decision.
pub fn reverse_all(
    effects: &[WorkspaceEffect],
    workspace_root: Option<&Path>,
    store: &dyn PreImageStore,
) -> Vec<(WorkspaceEffectReversal, Reversal)> {
    // Newest first: two writes to one path must unwind through their own
    // history (see the module doc's ordering note).
    let mut ordered: Vec<&WorkspaceEffect> = effects.iter().collect();
    ordered.sort_by_key(|effect| std::cmp::Reverse(effect.id));

    ordered
        .into_iter()
        .map(|effect| {
            let outcome = reverse(effect, workspace_root, store);
            let stamp = match &outcome {
                Reversal::Restored(_) => WorkspaceEffectReversal::restored(effect.id),
                Reversal::Refused(refusal) => {
                    WorkspaceEffectReversal::refused(effect.id, refusal.reason.clone())
                }
                // A skipped row is not touched: it was never addressable, so
                // stamping it would claim a reversal decision nobody made.
                Reversal::Skipped => WorkspaceEffectReversal {
                    effect_id: effect.id,
                    restored: false,
                    note: "skipped: not reversible".to_string(),
                },
            };
            (stamp, outcome)
        })
        .collect()
}

/// Render a journal path the way the user sees it: workspace-relative when it is
/// inside the workspace, absolute otherwise.
fn display_path(path: Option<&str>, workspace_root: Option<&Path>) -> String {
    let Some(path) = path else {
        return "<no path>".to_string();
    };
    if let Some(root) = workspace_root
        && let Ok(relative) = Path::new(path).strip_prefix(root)
        && !relative.as_os_str().is_empty()
    {
        return relative.to_string_lossy().into_owned();
    }
    path.to_string()
}

fn changed_file(
    effect: &WorkspaceEffect,
    path: String,
    _absolute: &Path,
    created: bool,
    deleted: bool,
) -> ChangedFile {
    ChangedFile {
        path,
        tool: effect.tool_name.clone(),
        round_id: effect.round_id.clone(),
        line_counts: None,
        reason: None,
        created,
        deleted,
    }
}

#[cfg(test)]
mod tests;
