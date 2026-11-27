//! Request-body relief: the byte dimension of context pressure.
//!
//! Every other budget in the harness is denominated in **tokens**, because that
//! is what the model's context window charges. A request has a second,
//! independent limit: the bytes an edge will accept. The two disagree most
//! sharply on attachments, which are ~1 600 tokens but hundreds of kilobytes
//! each, so a session can sit comfortably inside a million-token window and
//! still be refused as a 55 MB body.
//!
//! This module owns the relief for that second limit, and only that: the pure
//! selection policy lives in [`nuo_protocol::pressure`] (which messages, in what
//! order, keeping which attachments), and this module supplies the two things a
//! pure layer cannot — **durable artifact writes** for what the projection
//! drops, and the durable checkpoint that records the projection.
//!
//! # Why the bytes are written before they are dropped
//!
//! The invoice that replaces an attachment prints an `Artifact Path`. A path is
//! a promise that a producer wrote the bytes there; printing a path nobody
//! wrote is a claim the system cannot honour, and it makes the model's only
//! recovery (`read` that path) fail. [`persist_media_artifacts`] runs before the
//! projection is applied for exactly that reason (`[INV-SPOOL-01]`).

use std::io::Write as _;
use std::path::Path;

use base64::Engine as _;
use nuo_protocol::pressure::MediaOffload;

/// Persist every attachment a payload plan is about to drop, and return only
/// the items that were fully persisted.
///
/// Idempotent and atomic: the same bytes hash to the same path, and a partial
/// file can never appear at the final location (write to a unique sibling, then
/// rename). An item whose bytes could not be written is **dropped from the
/// plan**: its invoice would name a path nobody wrote, so the projection must
/// keep those pixels rather than leave the model a dead end. The caller then
/// either relieves with the remaining items or surfaces the refusal.
pub async fn persist_media_artifacts(plan: Vec<MediaOffload>) -> Vec<MediaOffload> {
    let mut persisted = Vec::with_capacity(plan.len());
    for item in plan {
        let mut complete = true;
        for (image, path) in item.removed.iter().zip(&item.artifact_paths) {
            let encoded = image.data.clone();
            let target = path.clone();
            let mime = image.mime.clone();
            let written = tokio::task::spawn_blocking(move || {
                let Some(bytes) = decode_base64(&encoded) else {
                    return Err(format!("{mime} attachment was not valid base64"));
                };
                write_atomic(Path::new(&target), &bytes)
            })
            .await;
            match written {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::warn!(path = %path, %error, "could not persist an offloaded attachment");
                    complete = false;
                }
                Err(error) => {
                    tracing::warn!(path = %path, %error, "artifact write task failed");
                    complete = false;
                }
            }
        }
        if complete {
            persisted.push(item);
        }
    }
    persisted
}

fn decode_base64(data: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(data).ok()
}

/// Write `bytes` to `path` atomically (unique sibling, fsync, rename).
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    let Some(parent) = path.parent() else {
        return Err(format!("invalid artifact path {}", path.display()));
    };
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let unique = format!(
        ".tmp.{}.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0),
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("artifact")
    );
    let temporary = parent.join(unique);
    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("could not write {}: {error}", path.display()));
    }
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        // A concurrent writer may have completed the same hash first.
        if path.exists() {
            return Ok(());
        }
        return Err(format!("could not install {}: {error}", path.display()));
    }
    Ok(())
}

/// How many payload relief passes one round may run.
///
/// Each pass is deliberate, not incremental: the provider told us only that the
/// body was too large, so the first correction aims a quarter below the size
/// that failed. A route that still refuses after three passes is not shrinking,
/// and another 40 MB upload cannot teach it anything.
pub const MAX_PAYLOAD_RELIEFS: u8 = 3;

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_tool::ImagePart;

    fn image(data: &str) -> ImagePart {
        ImagePart {
            mime: "image/png".into(),
            data: data.into(),
        }
    }

    #[test]
    fn an_atomic_write_is_idempotent_and_leaves_no_temporary_file() {
        let directory = std::env::temp_dir().join(format!("nuo-artifact-{}", uuid::Uuid::new_v4()));
        let path = directory.join("a.png");
        write_atomic(&path, b"first").expect("write");
        write_atomic(&path, b"second").expect("second write is a no-op");
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        let leftovers = std::fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".tmp."))
            .count();
        assert_eq!(leftovers, 0, "no temporary file may survive the rename");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[tokio::test]
    async fn artifacts_are_persisted_where_the_invoice_says_they_are() {
        use base64::Engine as _;
        let directory =
            std::env::temp_dir().join(format!("nuo-artifact-plan-{}", uuid::Uuid::new_v4()));
        let path = directory.join("offloaded.png");
        let plan = vec![MediaOffload {
            index: 0,
            removed: vec![image(
                &base64::engine::general_purpose::STANDARD.encode(b"pixels"),
            )],
            artifact_paths: vec![path.display().to_string()],
            kept: Vec::new(),
            content: String::new(),
            invoice: String::new(),
            bytes: 8,
        }];
        let persisted = persist_media_artifacts(plan).await;
        assert_eq!(persisted.len(), 1, "a written artifact stays in the plan");
        assert_eq!(std::fs::read(&path).unwrap(), b"pixels");
        let _ = std::fs::remove_dir_all(&directory);
    }
}
