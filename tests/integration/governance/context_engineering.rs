//! Context-engineering governance (ADR-0215, ADR-0216, ADR-0217).
//!
//! These are *module-graph and vocabulary* checks. The behavioural halves live
//! where they can be tested directly:
//!
//! - `[INV-CTXSRC-01]` duplicate topic: `nuo_engine::cognition::context::tests`.
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
    let engine = root.join("crates/nuo-engine/src");
    let mut violations = Vec::new();

    // The retired module must stay deleted.
    if engine.join("cognition/budget").exists() {
        violations.push("crates/nuo-engine/src/cognition/budget was reintroduced".to_string());
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
                    "{rel}:{}: derives a threshold from the window outside nuo_protocol::pressure",
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
    let context = root.join("crates/nuo-engine/src/cognition/context");
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
    let inputs = read(&root.join("crates/nuo-engine/src/cognition/context/inputs.rs"));

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

    let engine = root.join("crates/nuo-engine/src");
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
    let context = root.join("crates/nuo-engine/src/cognition/context");
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
                "crates/nuo-engine/src/cognition/context/{stem}.rs is not declared in mod.rs"
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
        "crates/nuo-protocol/src/request_projection.rs",
        "crates/nuo-protocol/src/context/plan.rs",
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
    let assembler = read(&root.join("crates/nuo-engine/src/cognition/model_request/mod.rs"));
    assert!(
        assembler.contains("plan.fixed"),
        "the request must be built from the measured plan ([INV-CTXPLAN-01])"
    );
    assert!(
        !assembler.contains("policy.plan(") || assembler.contains("fn plan("),
        "the assembler measures once; a second measurement would be a second truth"
    );
    let policy_file = read(&root.join("crates/nuo-engine/src/cognition/context/registry.rs"));
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
        "crates/nuo-protocol/src/request_projection.rs",
        "crates/nuo-protocol/src/transcript.rs",
        "crates/nuo-protocol/src/context/plan.rs",
        "crates/nuo-persistence/src/db/usage_projection.rs",
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
    let plan = read(&root.join("crates/nuo-protocol/src/context/plan.rs"));
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
        "crates/nuo-engine/src/cognition/context/inspect.rs",
        "crates/nuo-server/src/handlers/handlers_thread.rs",
        "crates/nuo-server/src/handlers/handlers_slash/commands/misc.rs",
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

// ── Envelope, tool-result header, and untrusted framing (ADR-0225..0229) ─────

fn production_source(path: &Path) -> String {
    let source = read(path);
    source
        .split("#[cfg(test)]")
        .next()
        .unwrap_or(&source)
        .to_owned()
}

/// `[INV-ENVELOPE-01]` (ADR-0226): no file outside `nuo_tool::envelope` writes
/// message markup with `format!`.
///
/// Six hand-written `format!` calls were how two readers drifted from their
/// writers. The one renderer is the fix, so the source scan is what keeps it the
/// one renderer.
#[test]
fn inv_envelope_01_no_injection_site_writes_markup_by_hand() {
    let root = workspace_root();
    let mut violations = Vec::new();
    for path in rust_files(&root.join("crates")) {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if rel.contains("/tests/")
            || rel.ends_with("/tests.rs")
            || rel.ends_with("_tests.rs")
            || rel.ends_with("crates/nuo-tool/src/envelope.rs")
        {
            continue;
        }
        let production = production_source(&path);
        for needle in [
            "\"<skill ",
            "\"<file ",
            "\"<project-rules",
            "\"<checkpoint ",
            "\"<subagent-result ",
            "\"<untrusted ",
            "\"<conversation_to_compact",
            "\"<prior_summary",
            "\"<additional_context",
            "\"<active_todo_list",
        ] {
            if production.contains(needle) {
                violations.push(format!("{rel}: writes `{needle}` markup by hand"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "one renderer for every message envelope ([INV-ENVELOPE-01], ADR-0226): {violations:#?}"
    );
}

/// `[INV-ENVELOPE-02]` (ADR-0226): no reader searches for a tag literal.
#[test]
fn inv_envelope_02_no_reader_searches_for_tag_literals() {
    let root = workspace_root();
    let mut violations = Vec::new();
    for path in rust_files(&root.join("crates")) {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if rel.contains("/tests/")
            || rel.ends_with("/tests.rs")
            || rel.ends_with("_tests.rs")
            || rel.ends_with("crates/nuo-tool/src/envelope.rs")
        {
            continue;
        }
        let production = production_source(&path);
        for needle in ["find(\"<skill name=", "starts_with(\"<file "] {
            if production.contains(needle) {
                violations.push(format!("{rel}: re-derives an envelope with `{needle}`"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "reader and writer share one parser ([INV-ENVELOPE-02], ADR-0226): {violations:#?}"
    );
}

/// `[INV-TOOLHEADER-01]`/`02` (ADR-0227): the header has one producer and one
/// reader.
#[test]
fn inv_toolheader_01_the_header_is_produced_and_read_in_one_place() {
    let root = workspace_root();
    let mut violations = Vec::new();
    for path in rust_files(&root.join("crates")) {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        // Skip test-only modules: `*/tests/*.rs` and `<name>/tests.rs`.
        if rel.contains("/tests/") || rel.ends_with("/tests.rs") || rel.ends_with("_tests.rs") {
            continue;
        }
        if rel.ends_with("crates/nuo-tool/src/tool_result.rs")
            || rel.ends_with("crates/nuo-tool/src/envelope.rs")
            || rel.ends_with("crates/nuo-tool/src/untrusted.rs")
        {
            continue;
        }
        let production = production_source(&path);
        for needle in [
            "result]:\\n",
            "result]:\")",
            "split_once(\" result]:\")",
            // No provider may invent a sixth spelling of the header.
            "[Tool result",
            "result for call",
        ] {
            if production.contains(needle) {
                violations.push(format!("{rel}: spells the tool-result header (`{needle}`)"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "one producer, one reader for the tool-result header ([INV-TOOLHEADER-01], ADR-0227): \
         {violations:#?}"
    );
}

/// `[INV-UNTRUSTED-01]`/`05` (ADR-0228): one framing, and the prompt describes
/// the region instead of a retired bracket fence.
#[test]
fn inv_untrusted_01_one_framing_and_no_prose_format() {
    let root = workspace_root();
    let mut violations = Vec::new();

    // The retired fence must not reappear in production code.
    for directory in ["crates"] {
        for path in rust_files(&root.join(directory)) {
            if path.file_name().is_some_and(|name| name == "tests.rs") {
                continue;
            }
            let rel = path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if rel.starts_with("docs/") || rel.ends_with("crates/nuo-tool/src/untrusted.rs") {
                continue;
            }
            let production = production_source(&path);
            for needle in ["BEGIN UNTRUSTED WEB CONTENT", "UNTRUSTED_PREFIX"] {
                if production.contains(needle) {
                    violations.push(format!("{rel}: reintroduces the retired fence"));
                }
            }
        }
    }

    // A context source may name `<untrusted>` and its semantics; it may not
    // describe a bracket fence to the model.
    let builtin = read(&root.join("crates/nuo-engine/src/cognition/context/builtin.rs"));
    assert!(
        builtin.contains("<untrusted>"),
        "the security block must name the region the model will see"
    );

    assert!(
        violations.is_empty(),
        "untrusted content has one framing ([INV-UNTRUSTED-01], ADR-0228): {violations:#?}"
    );
}

/// `[INV-CTXTOPIC-01]`/`02` (ADR-0229): no declared topic or input key is
/// unreachable from a production source.
#[test]
fn inv_ctxtopic_01_every_declared_topic_and_key_has_a_producer() {
    let root = workspace_root();
    let context = root.join("crates/nuo-engine/src/cognition/context");
    let builtin = read(&context.join("builtin.rs"));
    let inputs = read(&context.join("inputs.rs"));

    // Every `InputKey` variant must be declared by a default source.
    let mut keys = Vec::new();
    let mut in_enum = false;
    for line in inputs.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("pub enum InputKey") {
            in_enum = true;
            continue;
        }
        if in_enum {
            if trimmed == "}" {
                break;
            }
            if let Some(name) = trimmed.strip_suffix(',')
                && !name.starts_with("///")
                && !name.is_empty()
            {
                keys.push(name.to_owned());
            }
        }
    }
    assert!(keys.len() >= 8, "the InputKey scan found only {keys:?}");

    let mut violations = Vec::new();
    for key in &keys {
        if !builtin.contains(&format!("InputKey::{key}")) {
            violations.push(format!(
                "`InputKey::{key}` is declared by no default source"
            ));
        }
    }

    // Every `Topic` must be claimed. There is no exception list: ADR-0231
    // withdrew the one reservation this scan used to allow.
    let protocol = read(&root.join("crates/nuo-protocol/src/context/mod.rs"));
    let mut topics = Vec::new();
    let mut in_topic = false;
    for line in protocol.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("pub enum Topic") {
            in_topic = true;
            continue;
        }
        if in_topic {
            if trimmed == "}" {
                break;
            }
            if let Some(name) = trimmed.strip_suffix(',')
                && !name.starts_with("///")
                && !name.is_empty()
            {
                topics.push(name.to_owned());
            }
        }
    }
    assert!(topics.len() >= 12, "the Topic scan found only {topics:?}");
    for topic in &topics {
        if !builtin.contains(&format!("Topic::{topic}")) {
            violations.push(format!("`Topic::{topic}` has no production owner"));
        }
    }

    assert!(
        violations.is_empty(),
        "every context element has a production owner ([INV-CTXTOPIC-01], ADR-0229): \
         {violations:#?}"
    );
}

// ── MCP ownership and skill-tool reachability (ADR-0230) ─────────────────────

/// `[INV-MCP-01]`/`02` (ADR-0230): one MCP client and one MCP tool adapter.
#[test]
fn inv_mcp_01_one_client_and_one_adapter() {
    let root = workspace_root();
    let mut violations = Vec::new();

    // The protocol crate is vocabulary only.
    for banned in [
        "crates/mcp/src/client.rs",
        "crates/mcp/src/server.rs",
        "crates/mcp/src/adapter.rs",
        "crates/mcp/src/transport",
    ] {
        if root.join(banned).exists() {
            violations.push(format!("{banned}: a second MCP runtime was reintroduced"));
        }
    }

    // The parallel bridge must stay deleted.
    if root
        .join("crates/nuo-server/src/runtime/mcp/substrate_bridge.rs")
        .exists()
    {
        violations.push(
            "crates/nuo-server/src/runtime/mcp/substrate_bridge.rs: the parallel bridge is back"
                .to_string(),
        );
    }

    // Exactly one `impl Tool for` an MCP adapter.
    let mut adapters = Vec::new();
    for directory in ["crates/mcp/src", "crates/nuo-server/src/runtime/mcp"] {
        for path in rust_files(&root.join(directory)) {
            let source = read(&path);
            if source.contains("impl Tool for Mcp") {
                adapters.push(
                    path.strip_prefix(&root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .to_string(),
                );
            }
        }
    }
    if adapters.len() > 1 {
        violations.push(format!("two MCP tool adapters: {adapters:?}"));
    }

    assert!(
        violations.is_empty(),
        "one MCP client and one adapter ([INV-MCP-01], ADR-0230): {violations:#?}"
    );
}

/// `[INV-SKILLTOOL-01]` (ADR-0230): an `Overflow::Index` resume must name a tool
/// the composition root installs, and that tool must declare a `BuiltinTool`.
#[test]
fn inv_skilltool_01_every_resume_names_a_reachable_tool() {
    let root = workspace_root();
    let builtin = read(&root.join("crates/nuo-engine/src/cognition/context/builtin.rs"));
    let bootstrap = read(&root.join("crates/nuo-server/src/composition/bootstrap.rs"));
    let declared = read(&root.join("crates/nuo-tool/src/builtin.rs"));

    // Every resume target the default policy names.
    let mut resume_targets = Vec::new();
    for line in builtin.lines() {
        for marker in [r#"index_to_tool("#, r#"Budget::index_to_tool("#] {
            let _ = marker;
        }
        if let Some(rest) = line.split(r#"index_to_tool("#).nth(1)
            && let Some(name) = rest.split('"').nth(1)
        {
            resume_targets.push(name.to_owned());
        }
    }
    resume_targets.sort_unstable();
    resume_targets.dedup();
    assert!(
        !resume_targets.is_empty(),
        "the default policy declares no index resume; the scan is wrong"
    );

    // Reachability has exactly two shapes, and both must be visible here:
    //   (a) the composition root installs the tool by name, or
    //   (b) a production `Tool` impl declares the matching `BuiltinTool` variant
    //       and the root wires that crate's factory.
    let tool_crates = [
        "crates/nuo-tool-fs/src",
        "crates/nuo-tool-exec/src",
        "crates/nuo-tool-ast/src",
        "crates/nuo-engine/src/cognition/skills",
    ]
    .iter()
    .flat_map(|directory| rust_files(&root.join(directory)))
    .map(|path| read(&path))
    .collect::<Vec<_>>()
    .join("\n");
    let root_calls_a_factory = bootstrap.contains("create_fs_tools")
        || bootstrap.contains("create_exec_tools")
        || bootstrap.contains("install_tool");
    let produced = |name: &str, variant: &str| -> bool {
        bootstrap.contains(name)
            || (root_calls_a_factory && tool_crates.contains(&format!("BuiltinTool::{variant}")))
    };

    let mut violations = Vec::new();
    for tool in &resume_targets {
        let variant = tool
            .split('_')
            .map(|part| {
                let mut chars = part.chars();
                match chars.next() {
                    Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                    None => String::new(),
                }
            })
            .collect::<String>();
        if !declared.contains(&variant) {
            violations.push(format!("`{tool}` has no `BuiltinTool::{variant}`"));
        }
        if !produced(tool, &variant) {
            violations.push(format!(
                "`{tool}` is never installed by the composition root"
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "every resume route names a reachable tool ([INV-SKILLTOOL-01], ADR-0230): {violations:#?}"
    );
}

// ── Injection and envelope vocabularies have producers (ADR-0226, ADR-0232) ──

/// `[INV-INJECT-01]` (ADR-0232): every `InjectionKind` variant has a production
/// constructor.
///
/// The enum's own doc comment used to be the only guarantee ("each variant maps
/// 1:1 to a concrete injection site"), and two variants had already outlived
/// their producers: `SessionReviewInput` (a retired `/review` diagnostic held
/// open "so old session files deserialize", though `origin` is an `Option` with
/// `serde(default)`) and `SystemPrompt` (stamped only by a debug preview). Both
/// are gone, and this scan is what keeps the next one from arriving.
#[test]
fn inv_inject_01_every_injection_kind_has_a_producer() {
    let root = workspace_root();
    let declared = read(&root.join("crates/nuo-tool/src/message.rs"));
    let all = declared
        .split("pub const ALL: &'static [InjectionKind] = &[")
        .nth(1)
        .and_then(|rest| rest.split("];").next())
        .expect("InjectionKind::ALL");

    let mut variants = Vec::new();
    for line in all.lines() {
        let line = line.trim().trim_end_matches(',');
        if let Some(name) = line.strip_prefix("InjectionKind::") {
            variants.push(name.split('(').next().unwrap_or(name).to_owned());
        }
    }
    assert!(
        variants.len() >= 10,
        "the InjectionKind scan found only {variants:?}"
    );

    // A producer is a construction outside the declaration, the tag accessor,
    // and the tests. `ALL` itself names every variant, so the block that holds it
    // is stripped before the scan — otherwise the list would vouch for itself.
    let corpus = production_corpus_without_all(&root, &[]);

    let mut violations = Vec::new();
    for variant in &variants {
        if !injection_kind_has_producer(&corpus, variant) {
            violations.push(format!(
                "`InjectionKind::{variant}` has no production constructor (a reader or an \
                 accessor does not count)"
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "every injection kind has a producer ([INV-INJECT-01], ADR-0232): {violations:#?}"
    );
}

/// Whether `variant` has a production constructor in `corpus`.
///
/// A **producer**, not a mention. Rust puts the callee before its arguments, so
/// the enclosing call head is *behind* the occurrence:
/// `hidden_user(InjectionKind::X, ..)`. Scanning backwards over a bounded window
/// finds the head without pretending to parse. Three kinds of occurrence do not
/// count:
///
/// - a comparison (`==` / `!=` / `matches!`) is a *reader*: it asks which kind a
///   row carries and can never create one;
/// - the `tag()` accessor is the declaration spelling itself;
/// - the `ALL` block was already stripped from the corpus.
///
/// The scan is a heuristic, and its failure direction is deliberate: a producer
/// routed through an unknown helper is a false failure, never a silent pass. The
/// negative test below pins that direction.
fn injection_kind_has_producer(corpus: &str, variant: &str) -> bool {
    const PRODUCERS: &[&str] = &[
        "Message::injected",
        "Message::hidden",
        "Message::new",
        "hidden_user(",
        "hidden_user_with_reason(",
        "visible_user(",
        "InjectionOrigin::new",
    ];
    const WINDOW: usize = 160;
    let needle = format!("InjectionKind::{variant}");
    corpus.match_indices(&needle).any(|(index, _)| {
        let line_start = corpus[..index].rfind('\n').map_or(0, |newline| newline + 1);
        let line_end = corpus[index..]
            .find('\n')
            .map_or(corpus.len(), |n| index + n);
        let line = &corpus[line_start..line_end];
        if line.contains("==") || line.contains("!=") || line.contains("matches!") {
            return false;
        }
        let window_start = index.saturating_sub(WINDOW);
        let head = &corpus[window_start..index];
        PRODUCERS.iter().any(|producer| head.contains(producer))
    })
}

/// The producer scan is not vacuous.
///
/// A scan that passes on any string would certify a dead vocabulary. This pins
/// both directions: a real constructor counts, a reader and a string literal do
/// not.
#[test]
fn the_producer_scan_rejects_a_dead_variant() {
    let live = "fn make() { Message::injected(InjectionKind::Live, ..); }";
    assert!(injection_kind_has_producer(live, "Live"));
    let reader = "fn read(k: InjectionKind) { matches!(k, InjectionKind::Dead); }";
    assert!(!injection_kind_has_producer(reader, "Dead"));
    let mention = "let name = \"InjectionKind::Dead\";";
    assert!(!injection_kind_has_producer(mention, "Dead"));
}

/// `[INV-ENVELOPE-07]` (ADR-0226): every `Tag` member has a production
/// constructor.
///
/// `Tag::SystemReminder` and `Tag::Checkpoint` shipped with no door that could
/// produce them; `Checkpoint` then kept a hand-written checkpoint format alive in
/// `nuo-protocol` for a release. The tag vocabulary is the set of regions the harness
/// can actually emit, and this scan says so.
#[test]
fn inv_envelope_07_every_tag_has_a_producer() {
    let root = workspace_root();
    let envelope = read(&root.join("crates/nuo-tool/src/envelope.rs"));
    let all = envelope
        .split("pub const ALL: &'static [Tag] = &[")
        .nth(1)
        .and_then(|rest| rest.split("];").next())
        .expect("Tag::ALL");

    let mut tags = Vec::new();
    for line in all.lines() {
        let line = line.trim().trim_end_matches(',');
        if let Some(name) = line.strip_prefix("Tag::") {
            tags.push(name.to_owned());
        }
    }
    assert!(tags.len() >= 5, "the Tag scan found only {tags:?}");

    let corpus = production_corpus_without_all(&root, &[]);

    let mut violations = Vec::new();
    for tag in &tags {
        // A constructor, not a mention. `Tag::Checkpoint` appears in the
        // declaration, the parser, and diagnostics; only `Envelope::new(…)`
        // proves a door can emit it.
        let direct = format!("Envelope::new(Tag::{tag})");
        let qualified = format!("Envelope::new(nuo_tool::Tag::{tag})");
        if !corpus.contains(&direct) && !corpus.contains(&qualified) {
            violations.push(format!(
                "`Tag::{tag}` has no production constructor (`Envelope::new` never opens it)"
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "every envelope tag has a producer ([INV-ENVELOPE-07], ADR-0226): {violations:#?}"
    );
}

/// Production text of every crate, with every `pub const … ALL … ]` block
/// removed.
///
/// The blocks have to go: a vocabulary constant lists every variant by
/// definition, so leaving it in would let each member vouch for itself and the
/// scan would pass on an empty codebase.
fn production_corpus_without_all(root: &Path, extra: &[&str]) -> String {
    // `crates/nuo-tool/src` is included on purpose: it *owns* the declarations and is
    // also where some producers live (`untrusted::frame`). The `ALL` blocks are
    // stripped below, so the declaration cannot vouch for itself.
    let mut directories = vec![
        "crates/nuo-tool/src",
        "crates/nuo-engine/src",
        "crates/nuo-protocol/src",
        "crates/nuo-server/src",
        "crates/nuo/src",
        "crates/nuo-persistence/src",
    ];
    directories.extend_from_slice(extra);

    let mut corpus = String::new();
    for directory in directories {
        for path in rust_files(&root.join(directory)) {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if rel.contains("/tests/") || rel.ends_with("/tests.rs") || rel.ends_with("_tests.rs") {
                continue;
            }
            let production = production_source(&path);
            let mut rest = production.as_str();
            while let Some(start) = rest.find("pub const ALL") {
                match rest[start..].find("];") {
                    // Keep the two lines before the constant so a `//!` doc block
                    // that follows it is still scanned.
                    Some(end) => rest = &rest[start + end + 2..],
                    None => break,
                }
            }
            corpus.push_str(rest);
            corpus.push('\n');
        }
    }
    corpus
}

/// The producer scans are not vacuous.
///
/// A scan that passes because its corpus excludes the code it is checking is
/// worse than no scan: it certifies a dead vocabulary. This asserts the shared
/// corpus actually contains the declarations, so a future "fix" that narrows the
/// directory list fails here instead of silently passing.
#[test]
fn the_producer_scan_corpus_contains_the_declarations() {
    let root = workspace_root();
    let corpus = production_corpus_without_all(&root, &[]);
    for producer in [
        "fn render_blocks",
        "fn frame(",
        "fn subagent_result(",
        "fn checkpoint_message(",
    ] {
        assert!(
            corpus.contains(producer),
            "the scan corpus omits `{producer}`; a narrow corpus certifies a dead vocabulary"
        );
    }
}
