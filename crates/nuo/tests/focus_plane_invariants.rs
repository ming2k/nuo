//! Peer focus plane invariants (ADR-0145).
//!
//! These are *module-graph and shape* checks plus the behaviour they imply.
//! The behavioural halves live where they can be driven directly
//! (`crate::shell::focus` unit tests, `session::tests` resolver cases,
//! `shell::input::tests::paste_escape`), and this file pins the two facts a
//! single keypress cannot demonstrate:
//!
//! - `[INV-FOCUS-03]`: the focus state is one value. A second field describing
//!   it would restore the unrepresentable combinations the model removed, and it
//!   would come back as two `pub` fields on `App`, which is what this scans for.
//! - `[INV-FOCUS-01]`: `Esc` has no focus-clearing arm anywhere in the scene
//!   schemes, so a future contributor cannot quietly reintroduce the asymmetric
//!   exit.
//!
//! Run with `cargo test -p nuo --lib focus_plane_invariants`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

fn workspace_root() -> PathBuf {
    nuo_governance::find_workspace_root()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Every `.rs` file under `dir`, recursively.
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "rs"))
        .collect()
}

/// `[INV-FOCUS-03]` **One value, one truth**.
///
/// The peer focus plane is `App::focus`. No other field, on `App` or on the
/// per-tab context, may describe which surface owns the keyboard: a second such
/// field could hold "an entry is highlighted while the composer is live", the
/// exact combination ADR-0145 made unrepresentable.
#[test]
fn inv_focus_03_focus_state_is_a_single_value() {
    let root = workspace_root();
    let shell = root.join("crates/nuo/src/shell");
    let mut offenders = Vec::new();
    for path in rust_files(&shell) {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let content = read(&path);
        for (idx, line) in content.lines().enumerate() {
            let t = line.trim();
            // Field declarations only (`pub foo: bool` / `pub bar: Option<..>`).
            let is_field = (t.starts_with("pub ") || t.contains(" pub "))
                && t.contains(':')
                && !t.contains("fn ")
                && !t.contains("const ");
            if !is_field {
                continue;
            }
            for legacy in ["focused_target", "transcript_focused"] {
                if t.contains(legacy) {
                    offenders.push(format!("{rel}:{}: {t}", idx + 1));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "[INV-FOCUS-03] the peer focus plane is one `Focus` value (ADR-0145); these fields \
         reintroduce a second source of truth:\n{}",
        offenders.join("\n")
    );
}

/// `[INV-FOCUS-01]` **`Esc` never crosses planes**.
///
/// The scene schemes resolve `Esc`; none of them may produce a focus-clearing or
/// plane-toggling action from it. The check reads the resolvers' source because
/// the property is "no such arm exists", which no single keystroke can prove for
/// the states a test does not construct.
#[test]
fn inv_focus_01_esc_has_no_focus_arm_in_the_scene_schemes() {
    let root = workspace_root();
    let session = read(&root.join("crates/nuo/src/shell/session.rs"));
    let body = session
        .split("fn resolve_esc(")
        .nth(1)
        .expect("resolve_esc exists")
        .split("\n}\n")
        .next()
        .expect("resolve_esc body");
    for forbidden in ["ToggleFocusPlane", "ClearFocusedTarget", "focused_target"] {
        assert!(
            !body.contains(forbidden),
            "[INV-FOCUS-01] `resolve_esc` must not cross or clear focus planes (found `{forbidden}`)"
        );
    }
}

/// `[INV-FOCUS-01]` **One symmetric crossing**.
///
/// `Shift-Tab` is the only explicit crossing, and it is registered as exactly one
/// command resolving from exactly one chord. A second crossing chord would make
/// the operator non-symmetric again.
#[test]
fn inv_focus_01_shift_tab_is_the_only_explicit_crossing() {
    use nuo::shell::keymap::{
        COMMAND_REGISTRY, CommandId, GlobalOverrides, Key, Scope, resolve_global_key,
    };

    assert_eq!(
        resolve_global_key(Key::BACKTAB),
        Some(CommandId::ToggleFocusPlane)
    );
    // The chord exists but cannot be reassigned: it is the remove half of the
    // `Tab` pair, so a remap would let the pair drift apart.
    let mut map = std::collections::HashMap::new();
    map.insert("focus".to_string(), "ctrl+g".to_string());
    let overrides = GlobalOverrides::from_config(&map);
    assert_eq!(
        overrides.effective_binding(CommandId::ToggleFocusPlane),
        Some(Key::BACKTAB),
        "the crossing chord is grammar, not a remappable shortcut"
    );
    // And no other chord may claim the command, and the command may claim no
    // other chord: one symmetric crossing means one chord, both directions.
    let declared: Vec<Key> = COMMAND_REGISTRY
        .iter()
        .filter(|spec| spec.id == CommandId::ToggleFocusPlane)
        .flat_map(|spec| spec.bindings.iter().copied())
        .collect();
    assert_eq!(
        declared,
        vec![Key::BACKTAB],
        "the crossing declares exactly one chord"
    );
    for spec in COMMAND_REGISTRY {
        if spec.scope != Scope::Global || spec.id == CommandId::ToggleFocusPlane {
            continue;
        }
        for key in spec.bindings {
            assert_ne!(
                *key,
                Key::BACKTAB,
                "another global command ({:?}) claims the crossing chord",
                spec.id
            );
        }
    }
}
