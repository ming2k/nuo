//! Governance, safety, and policy enforcement subsystem.
//!
//! Provides execution containment, permission gating, command safety checks,
//! interaction controllers, and trajectory loop supervision.

pub(crate) mod bash_policy;
pub mod guard;
pub mod interaction;
pub(crate) mod permission_policy;
pub(crate) mod permission_store;
pub(crate) mod scoped_tools;
pub(crate) mod shell_input;
pub mod stream_loop_detector;
pub mod trajectory_guard;

pub(crate) use scoped_tools::ScopedToolDisable;

pub use guard::{GuardAction, RoundGuardState};
pub use interaction::{InteractionConfig, InteractionController};
pub use stream_loop_detector::{DegeneratePattern, StreamLoopDetector};
pub use trajectory_guard::TrajectoryLoopGuard;

#[cfg(test)]
mod boundary_tests {
    //! `[INV-HARNESS-03]` (ADR-0100): the governance layer is a strict inner
    //! layer — it may be depended *upon* by cognition, never the reverse. This
    //! is enforced mechanically by scanning the module's own sources, so a
    //! regression fails the build rather than relying on code review.
    use std::fs;
    use std::path::{Path, PathBuf};

    /// Cognition-side modules the governance layer must never reference.
    ///
    /// Built with `concat!` so this very list does not appear in the scanned
    /// source as a literal dependency path (the scanner would otherwise flag
    /// its own definition).
    const FORBIDDEN_COGNITION_MODULES: &[&str] = &[
        concat!("crate::", "agent"),
        concat!("crate::", "orchestration"),
        concat!("crate::", "model_request"),
        concat!("crate::", "interop"),
        concat!("crate::", "execution"),
        concat!("crate::", "conversation_context"),
    ];

    fn collect_sources(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_sources(&path, out);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn governance_module_has_no_cognition_dependency() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("governance");
        let mut files = Vec::new();
        collect_sources(&root, &mut files);
        assert!(
            !files.is_empty(),
            "expected to scan governance sources at {root:?}"
        );

        let mut violations = Vec::new();
        for file in &files {
            let text = fs::read_to_string(file).expect("read governance source");
            for (index, line) in text.lines().enumerate() {
                // Comments are documentation, not dependencies.
                if line.trim_start().starts_with("//") {
                    continue;
                }
                for needle in FORBIDDEN_COGNITION_MODULES {
                    if line.contains(needle) {
                        violations.push(format!(
                            "{}:{} depends on cognition-side `{needle}`",
                            file.display(),
                            index + 1
                        ));
                    }
                }
            }
        }

        assert!(
            violations.is_empty(),
            "governance must never depend on cognition-side modules [INV-HARNESS-03]:\n{}",
            violations.join("\n")
        );
    }
}
