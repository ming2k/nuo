//! Context-engineering governance (ADR-0215, ADR-0216, ADR-0217).
//!
//! These are *module-graph and vocabulary* checks. The behavioural halves live
//! where they can be tested directly:
//!
//! - `[INV-CTXSRC-01]` duplicate topic: `engine::cognition::context::tests`.
//! - `[INV-CTXSRC-02]` budget enforcement: the same module's overflow tests.
//! - `[INV-CTXSRC-03]` derived stability: the same module's band tests.
//! - `[INV-TOOLSURFACE-03]` prefix immutability: the same module's
//!   deferred-activation test.
//!
//! This file owns the checks that a per-function test cannot make: that the
//! declaration surface has no dead field, that one budget vocabulary exists, and
//! that no prompt source reaches for a tool name.

// Integration test target: assertions fail by panicking, so the production
// no-panic lints are meaningless here (the exemption ends at the test boundary).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    nuo::dev::find_workspace_root()
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

/// `[INV-CTXSRC-07]` (ADR-0215): a context threshold is derived only in the
/// protocol pressure vocabulary.
///
/// The retired `cognition::budget` module declared a second, unreferenced budget
/// with hardcoded 60%/75% watermarks while the live policy derived its thresholds
/// from the model window. Two budget vocabularies is how a status bar and a
/// compaction gate end up disagreeing about the same model.
///
/// The check is deliberately narrow: a fraction of a *model window* is the
/// defect, not every decimal constant. A BM25 `b` parameter or a UI ratio is a
/// different quantity and must not be driven into the protocol crate.
#[test]
fn inv_ctxsrc_07_one_budget_vocabulary() {
    let root = workspace_root();
    let engine = root.join("crates/engine/src");
    let mut violations = Vec::new();

    // The retired module must stay deleted.
    if engine.join("cognition/budget").exists() {
        violations.push("crates/engine/src/cognition/budget was reintroduced".to_string());
    }

    // A threshold derived from the window is `window * fraction`. A percentage
    // *display* divides by the window and is not a threshold, so the pattern is
    // deliberately one-directional.
    static THRESHOLD: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)(window[a-z_]*(\s+as\s+f(?:32|64))?\s*\*\s*[0-9])|([0-9]\.[0-9]+\s*\*\s*window)",
        )
        .expect("threshold regex")
    });

    for path in rust_files(&engine) {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let source = read(&path);
        let production = source.split("#[cfg(test)]").next().unwrap_or(&source);
        for (number, line) in production.lines().enumerate() {
            if THRESHOLD.is_match(line) {
                violations.push(format!(
                    "{rel}:{}: derives a threshold from the window outside protocol::pressure",
                    number + 1
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "one budget vocabulary ([INV-CTXSRC-07], ADR-0215): {violations:#?}"
    );
}

/// `[INV-TOOLSURFACE-01]` (ADR-0216): a context source must not compare a tool
/// name.
///
/// The prompt policy used to branch on `name == "find_files"` and five siblings,
/// so renaming or aliasing a native tool silently deleted its guidance. Guidance
/// is now gated on `BuiltinTool` capability, and the name list is not reachable
/// from `cognition::context` at all.
#[test]
fn inv_toolsurface_01_no_tool_name_branch_in_context_sources() {
    let root = workspace_root();
    let context = root.join("crates/engine/src/cognition/context");
    let mut violations = Vec::new();
    for path in rust_files(&context) {
        // `tests.rs` is the module's own test suite: a test double resolves its
        // name so the real `Tool` contract is satisfied.
        if path.file_name().is_some_and(|name| name == "tests.rs") {
            continue;
        }
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let source = read(&path);
        let production = source.split("#[cfg(test)]").next().unwrap_or(&source);
        for needle in [".name() ==", ".name() !=", "== \"find_files\""] {
            if production.contains(needle) {
                violations.push(format!("{rel}: branches on a tool name (`{needle}`)"));
            }
        }
        // The prompt may *write* a tool's canonical name inside a sentence; it
        // may not resolve one. `as_str()` is the resolution path.
        if production.contains("BuiltinTool::") && production.contains(".as_str()") {
            violations.push(format!("{rel}: resolves a BuiltinTool back to a name"));
        }
    }
    assert!(
        violations.is_empty(),
        "the prompt branches on declared capability, not names \
         ([INV-TOOLSURFACE-01], ADR-0216): {violations:#?}"
    );
}

/// `[INV-CTXSRC-06]` (ADR-0215): every declared input has a production reader.
///
/// `InstructionContext::has_subagent_tool` was computed on every model request
/// and read by nobody; a `pub` field is invisible to `dead_code`. A declaration
/// surface that accumulates unread fields is a specification nobody implements.
#[test]
fn inv_ctxsrc_06_every_context_input_has_a_reader() {
    let root = workspace_root();
    let inputs = read(&root.join("crates/engine/src/cognition/context/inputs.rs"));

    // The field names of `ContextInputs`, in declaration order.
    let mut fields = Vec::new();
    let mut in_struct = false;
    for line in inputs.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("pub struct ContextInputs") {
            in_struct = true;
            continue;
        }
        if in_struct {
            if trimmed == "}" {
                break;
            }
            if let Some(rest) = trimmed.strip_prefix("pub ")
                && let Some((name, _)) = rest.split_once(':')
            {
                fields.push(name.trim().to_owned());
            }
        }
    }
    assert!(
        fields.len() >= 8,
        "the ContextInputs field scan found only {fields:?}"
    );

    let engine = root.join("crates/engine/src");
    let mut readers = Vec::new();
    for path in rust_files(&engine) {
        if path.ends_with("inputs.rs") {
            continue;
        }
        readers.push(read(&path));
    }
    let corpus = readers.join("\n");

    let mut violations = Vec::new();
    for field in fields {
        // A reader is either a field access or an `InputKey` variant whose name
        // matches the field's snake_case form.
        let accessed = corpus.contains(&format!(".{field}"));
        if !accessed {
            violations.push(format!("`ContextInputs::{field}` has no production reader"));
        }
    }
    assert!(
        violations.is_empty(),
        "no dead context declaration ([INV-CTXSRC-06], ADR-0215): {violations:#?}"
    );
}

/// `[INV-CTXSRC-06]` (ADR-0215): every `context` submodule is reachable from the
/// module root.
///
/// A file that no `mod` declaration names is compiled by nothing: no error, no
/// warning, no test. The retired `model_request/{instruction,policies}.rs`
/// survived exactly this way for one build.
#[test]
fn inv_ctxsrc_06_every_context_submodule_is_declared() {
    let root = workspace_root();
    let context = root.join("crates/engine/src/cognition/context");
    let module = read(&context.join("mod.rs"));
    let mut violations = Vec::new();
    for path in rust_files(&context) {
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        if stem == "mod" {
            continue;
        }
        let declared = module.contains(&format!("mod {stem};"));
        if !declared {
            violations.push(format!(
                "crates/engine/src/cognition/context/{stem}.rs is not declared in mod.rs"
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "every context submodule must be reachable ([INV-CTXSRC-06], ADR-0215): {violations:#?}"
    );
}

/// `[INV-CTXPLAN-03]` (ADR-0217): durable records never carry an in-process hash.
///
/// `DefaultHasher` output is not stable across builds. Persisting one would make
/// a stored prefix identity meaningless after the next toolchain upgrade, so the
/// durable record carries the SHA-256 prefix instead.
#[test]
fn inv_ctxplan_03_persisted_projection_carries_no_volatile_hash() {
    let root = workspace_root();
    let mut violations = Vec::new();
    for file in [
        "crates/protocol/src/request_projection.rs",
        "crates/protocol/src/context/plan.rs",
    ] {
        let source = read(&root.join(file));
        if source.contains("DefaultHasher") {
            violations.push(format!("{file}: durable record names DefaultHasher"));
        }
    }
    assert!(
        violations.is_empty(),
        "persisted identity must be stable across builds ([INV-CTXPLAN-03], ADR-0217): \
         {violations:#?}"
    );
}

/// `[INV-CTXPLAN-01]` (ADR-0217): the plan is the request.
///
/// The inspector renders `ContextPlan`; the assembler builds the request from
/// that same value. If the two ever diverge, an operator reads a prompt the
/// model never received. This check is structural — the assembler must consume a
/// plan, not re-render blocks itself.
#[test]
fn inv_ctxplan_01_the_assembler_builds_from_a_plan() {
    let root = workspace_root();
    let assembler = read(&root.join("crates/engine/src/cognition/model_request/mod.rs"));
    assert!(
        assembler.contains("plan.fixed") && assembler.contains("plan.transient"),
        "the request must be built from the measured plan ([INV-CTXPLAN-01])"
    );
    assert!(
        !assembler.contains("policy.plan(") || assembler.contains("fn plan("),
        "the assembler measures once; a second measurement would be a second truth"
    );
    let policy_file = read(&root.join("crates/engine/src/cognition/context/registry.rs"));
    assert!(
        policy_file.contains("fn plan_with_ceiling"),
        "the policy owns measurement"
    );
}

/// `[INV-CTXPLAN-06]` (ADR-0220): a rendered body is live only, never durable.
///
/// The whole point of carrying the prompt's own bytes over the wire is that it
/// costs nothing to keep — because nothing keeps it. A durable type that named
/// `PlannedBlockView` (or stored a `body`) would put the full prompt in the
/// database, which ADR-0217 discarded on cost grounds and which would assert
/// something false the moment the workspace moved.
#[test]
fn inv_ctxplan_06_the_rendered_body_is_never_durable() {
    let root = workspace_root();
    let mut violations = Vec::new();

    for file in [
        "crates/protocol/src/request_projection.rs",
        "crates/protocol/src/transcript.rs",
        "crates/protocol/src/context/plan.rs",
        "crates/persistence/src/db/usage_projection.rs",
    ] {
        let source = read(&root.join(file));
        for needle in ["PlannedBlockView", "ContextPlanView"] {
            if source.contains(needle) {
                violations.push(format!("{file}: durable module names `{needle}`"));
            }
        }
    }

    // The transport type may not assert a body field on the engine row either:
    // a `pub body: String` there is how the rendered prompt would find its way
    // into a record nobody re-reads.
    let plan = read(&root.join("crates/protocol/src/context/plan.rs"));
    let block_struct = plan
        .split("pub struct PlannedBlock {")
        .nth(1)
        .and_then(|rest| rest.split("\n}").next())
        .unwrap_or_default();
    assert!(
        !block_struct.contains("pub body:"),
        "the measured row must not carry bytes: the body lives on the view"
    );

    assert!(
        violations.is_empty(),
        "a rendered body is live only, never durable ([INV-CTXPLAN-06], ADR-0220): \
         {violations:#?}"
    );
}

/// `[INV-CTXPLAN-07]` (ADR-0220): one measurement, many readers.
///
/// `Policy::plan` is impure — it advances the prefix-drift witness — so a reader
/// that re-measured to answer a question would make the *next* real request
/// report drift against a prompt nobody sent. The only source any inspector may
/// read is the engine's stored measurement.
#[test]
fn inv_ctxplan_07_no_reader_re_measures_a_plan() {
    let root = workspace_root();
    let mut violations = Vec::new();
    // Every reader that renders a plan out of process.
    for file in [
        "crates/nuo/src/shell/overlays/context_plan.rs",
        "crates/engine/src/cognition/context/inspect.rs",
        "crates/server/src/handlers/handlers_thread.rs",
        "crates/server/src/handlers/handlers_slash/commands/misc.rs",
    ] {
        let source = read(&root.join(file));
        let production = source.split("#[cfg(test)]").next().unwrap_or(&source);
        if production.contains(".plan(") || production.contains("plan_with_ceiling(") {
            violations.push(format!(
                "{file}: a reader measures a plan instead of reading the stored measurement"
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "one measurement, many readers ([INV-CTXPLAN-07], ADR-0220): {violations:#?}"
    );
}
