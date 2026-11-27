//! The answer-language governance gates (ADR-0222).
//!
//! The behavioural halves live where they can be tested directly:
//!
//! - `[INV-LANG-02]` undeclared is not English:
//!   `nuo_engine::cognition::context::tests::an_undeclared_language_renders_no_block_at_all`.
//! - `[INV-LANG-03]` the rule outranks its surroundings:
//!   `nuo_engine::cognition::context::tests::a_declared_language_names_itself_and_disarms_the_english_context`.
//! - `[INV-LANG-04]` a switch moves only the session band:
//!   `nuo_engine::cognition::context::tests::changing_the_language_moves_only_the_session_band`.
//!
//! This file owns the checks that no single function can make: that every entry
//! point funnels through one validator, and that the declaration is inherited by
//! handle rather than copied.

// Integration test target: assertions fail by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    nuo_test_support::dev::find_workspace_root()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    out
}

/// `[INV-LANG-01]` (ADR-0222): a language that reaches the prompt is a validated
/// tag, and every surface that accepts one routes through the same validator.
///
/// A free-form string in `agent.toml` would be a prompt-injection surface with a
/// config file as the delivery vector, and three independent parsers (load,
/// `config check`, `/language`) would be three chances for one of them to accept
/// what the others refuse. This check is structural: the validator is declared in
/// the contract crate, and no consumer re-implements the shape check.
#[test]
fn inv_lang_01_every_entry_point_uses_the_one_language_validator() {
    let root = workspace_root();
    let validator = read(&root.join("crates/nuo-protocol/src/language.rs"));
    assert!(
        validator.contains("pub fn parse("),
        "the language validator must be a single declared constructor"
    );

    let mut violations = Vec::new();
    // The config loader, the `config check` gate, and the slash command must all
    // name the validator rather than validating inline.
    for file in [
        "crates/nuo-persistence/src/config/mod.rs",
        "crates/nuo-server/src/handlers/handlers_slash/commands/misc.rs",
    ] {
        let source = read(&root.join(file));
        if !source.contains("ResponseLanguage::parse") {
            violations.push(format!(
                "{file}: does not route through ResponseLanguage::parse"
            ));
        }
    }
    // And nothing may hand-roll the shape check: a second `is_control()` beside
    // the prompt renderer is how two validators drift.
    for path in rust_files(&root.join("crates/nuo-persistence/src/config")) {
        let source = read(&path);
        let production = source.split("#[cfg(test)]").next().unwrap_or(&source);
        if production.contains("response_language") && production.contains("is_control()") {
            violations.push(format!(
                "{}: re-implements the language shape check",
                path.display()
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "one language validator ([INV-LANG-01], ADR-0222): {violations:#?}"
    );
}

/// `[INV-LANG-05]` (ADR-0222): the declaration is inherited, never copied.
///
/// A subagent that got a *copy* of the parent's language would keep answering in
/// the old one after `/language` changed, and the human reading the child's report
/// is the same human who changed it. The same rule already governs user
/// instructions (ADR-0165), so the shape is not new — only the fact that this
/// fifth input follows it.
#[test]
fn inv_lang_05_the_subagent_binds_the_parent_handle() {
    let root = workspace_root();
    let run = read(&root.join("crates/nuo-engine/src/cognition/interop/subagent_tool/tool/run.rs"));
    assert!(
        run.contains("bind_response_language"),
        "a spawned subagent must bind the parent's language handle"
    );
    let bootstrap = read(&root.join("crates/nuo-server/src/composition/bootstrap.rs"));
    assert!(
        bootstrap.contains("bind_response_language(agent.response_language_handle())"),
        "the composition root must hand the subagent tool the parent's handle"
    );
    let tool =
        read(&root.join("crates/nuo-engine/src/cognition/interop/subagent_tool/tool/mod.rs"));
    assert!(
        tool.contains("parent_response_language"),
        "the subagent tool must own the parent handle it binds"
    );
}
