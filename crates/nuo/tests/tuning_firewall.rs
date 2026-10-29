//! Provider-owned model tunables — mechanical enforcement (ADR-0126).
//!
//! Integration test target: assertions fail by panicking, so the production
//! no-panic lints are meaningless here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

/// Read every `.rs` file under `dir`, recursively.
fn rust_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
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

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// `[INV-TUNE-01]` (ADR-0126): the framework core must not carry a **provider
/// wire encoding** or a **model-family branch** for reasoning depth.
///
/// Both defects shipped in this tree:
///
/// - `Effort::gemini_thinking_budget` encoded Google's `thinkingBudget` bucket
///   fractions in the *universal* depth vocabulary, contradicting
///   `docs/reference/effort.md`'s statement that this translation is
///   per-provider wire knowledge. It now lives in the provider that owns it
///   (`nuo-provider-google-antigravity::request::thinking_budget_wire`).
/// - `Effort::channel_default` picked a default by matching a family-name
///   string (`family == "gpt" || family.starts_with("gpt")`), so the framework
///   core had to be edited whenever a provider wanted a different default. The
///   preference is now provider data (`Model::default_effort`).
///
/// The check is deliberately narrow — it fails on the two specific patterns
/// rather than banning all family strings everywhere, because a provider crate
/// legitimately names its own families.
#[test]
fn test_inv_tune_01_no_vendor_wire_encoding_or_family_branch_in_core_vocabulary() {
    let root = nuo::dev::find_workspace_root();

    // The files that constitute the *framework's* depth vocabulary and its
    // resolution points. A wire encoding or a family branch here is the defect.
    let guarded = [
        "crates/nuo-provider/src/effort.rs",
        "crates/nuo-provider/src/tuning.rs",
        "crates/nuo-provider/src/model.rs",
        "crates/nuo-provider/src/dialects.rs",
    ];

    let mut problems = Vec::new();
    for file in guarded {
        let path = root.join(file);
        let Ok(content) = std::fs::read_to_string(&path) else {
            problems.push(format!("{file}: guarded file is missing"));
            continue;
        };
        for (idx, line) in content.lines().enumerate() {
            let line_num = idx + 1;
            let code = line.split("//").next().unwrap_or_default();
            if code.contains("gemini_thinking_budget") || code.contains("thinkingBudget") {
                problems.push(format!(
                    "{file}:{line_num}: provider wire encoding in the framework vocabulary; \
                     a budget/threshold translation belongs in the owning provider (ADR-0126)"
                ));
            }
            // A family-name comparison that selects a behaviour.
            for needle in [
                "family == \"",
                "family.starts_with(\"",
                "family.as_str() == \"",
            ] {
                if code.contains(needle) {
                    problems.push(format!(
                        "{file}:{line_num}: model-family branch in the framework core \
                         (`{needle}`); a per-family default is provider data \
                         (`Model::default_effort`) (ADR-0126)"
                    ));
                }
            }
        }
    }

    if !problems.is_empty() {
        panic!(
            "Framework tuning vocabulary carries provider-owned knowledge:\n{}",
            problems.join("\n")
        );
    }
}

/// `[INV-TUNE-04]` (ADR-0126): knob resolution happens **once**, in
/// `nuo-provider`. A presentation surface must render the server's answer, not
/// re-derive it.
///
/// Two real defects motivated this: the composer footer decided whether to show
/// depth by matching `protocol.as_str()` against `"anthropic"`, and the model
/// editor clamped/did-defaulted a value the server had already resolved
/// (`unwrap_or("medium")`, `clamp_to_levels`). Both are second implementations
/// of a rule the server owns, and both drifted from the wire.
#[test]
fn test_inv_tune_04_no_client_side_knob_resolution() {
    let root = nuo::dev::find_workspace_root();
    let shell = root.join("crates/nuo/src/shell");

    // Resolution primitives that must not appear in the presentation layer.
    let forbidden = ["clamp_to(", "clamp_to_levels(", "channel_default("];
    // Protocol strings used to *decide* knob presence. Comparing the protocol
    // for display/diagnostics is fine; branching on it to infer capability is
    // not. These are the exact comparisons the legacy code used.
    let forbidden_protocol_branches = ["\"anthropic\" =>", "\"openai\" =>", "\"google\" =>"];

    let mut problems = Vec::new();
    for path in rust_files(&shell) {
        let rel_path = rel(&root, &path);
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (idx, line) in content.lines().enumerate() {
            let line_num = idx + 1;
            let code = line.split("//").next().unwrap_or_default();
            for needle in forbidden {
                if code.contains(needle) {
                    problems.push(format!(
                        "{rel_path}:{line_num}: presents a knob by re-deriving it \
                         (`{needle}`); read the server's resolved value instead (ADR-0126)"
                    ));
                }
            }
            for needle in forbidden_protocol_branches {
                if code.contains(needle) {
                    problems.push(format!(
                        "{rel_path}:{line_num}: infers knob presence from a protocol string \
                         (`{needle}`); knob existence is `ProviderModelInfo::tunables` (ADR-0126)"
                    ));
                }
            }
        }
    }

    if !problems.is_empty() {
        panic!(
            "Presentation layer re-derives provider-owned tuning:\n{}",
            problems.join("\n")
        );
    }
}

/// `[INV-TUNE-03]` (ADR-0126): one fact, one carrier.
///
/// A per-route tuning choice lives in the **open map** and nowhere else. This
/// check exists because the first implementation of ADR-0126 kept typed
/// `effort`/`thinking` fields *beside* `RouteSettings::tunings` — "mirrors".
/// Two writable copies of one fact drift: a writer that updates one and not the
/// other produces a route whose stored depth and effective depth disagree, with
/// no error anywhere.
///
/// The retired on-disk spelling is **not** a typed field; it is translated
/// explicitly by the migration reader (`read_legacy_cache_route_settings`), which
/// reads a `serde_json::Value` because `deny_unknown_fields` would otherwise
/// reject the whole record and silently drop the user's overrides.
#[test]
fn test_inv_tune_03_no_typed_knob_mirrors_beside_the_open_map() {
    let root = nuo::dev::find_workspace_root();
    // Only the **persisted** carrier is guarded. `ProviderModelInfo::effort` /
    // `thinking` are not mirrors: they are the server's *computed projection*
    // of `Channel::effective_effort` / `thinking_state`, which are in turn read
    // from this one map. A projection has one source and cannot disagree with
    // itself; a stored mirror is a second source and can.
    let targets = [
        "crates/nuo-persistence/src/config.rs",
        // A *declaration* is the same hazard in the model registry: whether a
        // route has a thinking switch is derivable from its reasoning encoding
        // (`ReasoningSupport::has_thinking_switch`), so a parallel
        // `thinking_switch: bool` field would be a second source that can drift
        // (it did exist and was removed).
        "crates/nuo-provider/src/model.rs",
    ];

    let mut problems = Vec::new();
    for file in targets {
        let path = root.join(file);
        let Ok(content) = std::fs::read_to_string(&path) else {
            problems.push(format!("{file}: guarded file is missing"));
            continue;
        };
        for (idx, line) in content.lines().enumerate() {
            let line_num = idx + 1;
            let code = line.split("//").next().unwrap_or_default();
            // A *field declaration* of the retired spelling, i.e. `pub effort:`
            // or `pub thinking:` inside a persisted/DTO struct.
            // The retired *store* spellings. Note `pub thinking: ReasoningSupport`
            // is legitimate — that is the provider's capability declaration, the
            // single source `has_thinking_switch()` derives from — so the check
            // targets the boolean/string *tuning* forms only.
            for needle in [
                "pub effort:",
                "pub thinking: Option<bool>,",
                "pub thinking_switch:",
            ] {
                if code.contains(needle) {
                    problems.push(format!(
                        "{file}:{line_num}: typed knob mirror `{needle}` beside the open \
                         tuning map; a knob lives in `TunableSettings` only (ADR-0126)"
                    ));
                }
            }
        }
    }

    if !problems.is_empty() {
        panic!(
            "A per-route tuning fact has two carriers:\n{}",
            problems.join("\n")
        );
    }
}
