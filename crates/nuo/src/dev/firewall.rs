//! Presentation dependency firewall and tiered workspace topology verification.
//!
//! Enforces concentric dependency direction and tiered crate layout
//! (ADR-0005, ADR-0058, ADR-0103, ADR-0105, ADR-0106, ADR-0107, ADR-0109, ADR-0111).
//!
//! Rule summary:
//!   R1: Server purity — `nuo-server` must never depend on presentation/terminal substrates.
//!   R2: Anti-inversion & retired crate isolation.
//!   R3: Zero-IO presentation substrate (`nuo-ui-primitives`, `nuo-view`).
//!   R4: UI primitives purity (`nuo-ui-primitives` does not depend on `nuo-view`).
//!   R5: Pure tiered workspace topology (`[INV-ARCH-TIER-01]`).
//!   Lean: Lean CLI link gate (`[INV-ARCH-CLI-LEAN-LINK]`).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

const TIERS: &[&str] = &["crates", "providers", "tools"];
const SANCTIONED_MEMBERS: &[&str] = &["crates/*", "tools/*", "providers/*"];
const HOST_PATH: &str = "crates/nuo/Cargo.toml";

const TERMINAL_SUBSTRATE: &[&str] = &["nuotc", "crossterm"];
const PRESENTATION_CRATES: &[&str] = &["nuo-ui-primitives", "nuo-view"];
const RETIRED_CRATES: &[&str] = &[
    "nuo-client",
    "nuo-egress",
    "nuo-oauth",
    "nuo-present",
    "nuo-ui",
    "nuo-shell",
    "nuo-tui",
];

const UI_FORBIDDEN: &[&str] = &[
    "tokio-tungstenite", // async websocket transport
    "netune",            // outbound egress
    "crossterm",         // terminal raw-mode driver
    "arboard",           // clipboard
    "libc",              // raw OS calls (terminal/process)
    "dirs",              // filesystem layout
    "nuo-host",          // daemon process supervision
    "nuo-server",        // headless runtime
    "nuo-harness",       // execution engine
    "nuo-provider",      // provider registry
    "nuo-mcp",           // MCP client
];

#[derive(Debug, Clone)]
pub struct ManifestInfo {
    pub name: String,
    pub rel_path: String,
    pub shipped_deps: HashSet<String>,
}

fn has_bin_target(path: &Path, content: &str) -> bool {
    if content.contains("[[bin]]") {
        return true;
    }
    if let Some(parent) = path.parent() {
        if parent.join("src/main.rs").exists() {
            return true;
        }
    }
    false
}

fn parse_manifest(root: &Path, full_path: &Path) -> Result<ManifestInfo, String> {
    let content = fs::read_to_string(full_path)
        .map_err(|e| format!("Failed to read {}: {e}", full_path.display()))?;
    let rel_path = full_path
        .strip_prefix(root)
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .replace('\\', "/");

    let val: toml::Table =
        toml::from_str(&content).map_err(|e| format!("Failed to parse TOML in {rel_path}: {e}"))?;

    let name = val
        .get("package")
        .and_then(|p| p.as_table())
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("?")
        .to_string();

    let mut shipped_deps = HashSet::new();

    for section in &["dependencies", "build-dependencies"] {
        if let Some(table) = val.get(*section).and_then(|s| s.as_table()) {
            for (key, dep_val) in table {
                shipped_deps.insert(key.clone());
                if let Some(dep_tbl) = dep_val.as_table() {
                    if let Some(pkg) = dep_tbl.get("package").and_then(|p| p.as_str()) {
                        shipped_deps.insert(pkg.to_string());
                    }
                }
            }
        }
    }

    Ok(ManifestInfo {
        name,
        rel_path,
        shipped_deps,
    })
}

fn parse_root_members(content: &str) -> Vec<String> {
    if let Ok(val) = toml::from_str::<toml::Table>(content) {
        if let Some(ws) = val.get("workspace").and_then(|w| w.as_table()) {
            if let Some(arr) = ws.get("members").and_then(|m| m.as_array()) {
                return arr
                    .iter()
                    .filter_map(|item| item.as_str().map(|s| s.to_string()))
                    .collect();
            }
        }
    }
    Vec::new()
}

pub fn check_tier_violations(root: &Path) -> (Vec<String>, Vec<PathBuf>) {
    let mut problems = Vec::new();
    let mut crate_manifests = Vec::new();

    let root_manifest_path = root.join("Cargo.toml");
    let root_content = match fs::read_to_string(&root_manifest_path) {
        Ok(c) => c,
        Err(e) => {
            problems.push(format!("R5: failed to read root Cargo.toml: {e}"));
            return (problems, crate_manifests);
        }
    };

    let members = parse_root_members(&root_content);
    let sanctioned: Vec<String> = SANCTIONED_MEMBERS.iter().map(|s| s.to_string()).collect();
    if members != sanctioned {
        problems.push(format!(
            "R5: root Cargo.toml `members` is {:?}, expected {:?} ([INV-ARCH-TIER-01] wildcard admission)",
            members, sanctioned
        ));
    }

    let mut host_binaries = Vec::new();

    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            name != "target" && name != ".git" && name != "node_modules"
        })
        .flatten()
    {
        if entry.file_type().is_file() && entry.file_name() == "Cargo.toml" {
            let full_path = entry.path();
            if full_path == root_manifest_path {
                continue;
            }

            let rel = match full_path.strip_prefix(root) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            let parts: Vec<&str> = rel_str.split('/').collect();
            let depth = parts.len() - 1; // directory components above manifest

            let content = fs::read_to_string(full_path).unwrap_or_default();
            if has_bin_target(full_path, &content) {
                host_binaries.push(rel_str.clone());
            }

            if depth == 1 {
                problems.push(format!(
                    "R5: crate manifest '{rel_str}' sits at the workspace root; all crates must reside within a sanctioned tier ({}) ([INV-ARCH-TIER-01])",
                    TIERS.join(" | ")
                ));
                continue;
            }
            if depth > 2 {
                problems.push(format!(
                    "R5: crate manifest '{rel_str}' nests deeper than one tier; tiers are flat ([INV-ARCH-TIER-01])"
                ));
                continue;
            }

            let tier = parts[0];
            let crate_dir = parts[1];

            if !TIERS.contains(&tier) {
                problems.push(format!(
                    "R5: crate manifest '{rel_str}' is under unsanctioned tier '{tier}' ({}) ([INV-ARCH-TIER-01])",
                    TIERS.join(" | ")
                ));
                continue;
            }

            if tier == "crates" {
                if crate_dir != "nuo" && !crate_dir.starts_with("nuo-") {
                    problems.push(format!(
                        "R5: crate directory 'crates/{crate_dir}' must be 'nuo' (host binary) or carry prefix 'nuo-' ([INV-ARCH-TIER-01])"
                    ));
                }
            } else if tier == "providers" {
                if !crate_dir.starts_with("nuo-provider-") {
                    problems.push(format!(
                        "R5: crate directory '{tier}/{crate_dir}' must carry the tier namespace prefix 'nuo-provider-' ([INV-ARCH-TIER-01])"
                    ));
                }
            } else if tier == "tools" && !crate_dir.starts_with("nuo-tool-") {
                problems.push(format!(
                    "R5: crate directory '{tier}/{crate_dir}' must carry the tier namespace prefix 'nuo-tool-' ([INV-ARCH-TIER-01])"
                ));
            }

            crate_manifests.push(full_path.to_path_buf());
        }
    }

    host_binaries.sort();
    let expected_binaries = vec![HOST_PATH.to_string()];
    if host_binaries != expected_binaries {
        problems.push(format!(
            "R5: binary targets found in {:?}, expected exactly {:?} ([INV-ARCH-TIER-01] single public binary, [INV-CLI-01])",
            host_binaries, expected_binaries
        ));
    }

    (problems, crate_manifests)
}

pub fn check_presentation_firewall(root: &Path) -> Result<(), String> {
    let (mut problems, manifest_paths) = check_tier_violations(root);

    let mut manifests = HashMap::new();
    for path in manifest_paths {
        match parse_manifest(root, &path) {
            Ok(info) => {
                manifests.insert(info.name.clone(), info);
            }
            Err(e) => problems.push(e),
        }
    }

    let terminal_substrate: HashSet<&str> = TERMINAL_SUBSTRATE.iter().copied().collect();
    let presentation_crates: HashSet<&str> = PRESENTATION_CRATES.iter().copied().collect();
    let retired_crates: HashSet<&str> = RETIRED_CRATES.iter().copied().collect();
    let presentation_layer: HashSet<&str> = presentation_crates
        .union(&["nuo"].iter().copied().collect())
        .copied()
        .collect();
    let ui_forbidden: HashSet<&str> = UI_FORBIDDEN.iter().copied().collect();

    let mut sorted_manifest_names: Vec<String> = manifests.keys().cloned().collect();
    sorted_manifest_names.sort();

    for name in &sorted_manifest_names {
        let info = &manifests[name];
        let deps: HashSet<&str> = info.shipped_deps.iter().map(|s| s.as_str()).collect();

        // R1: Server purity
        if name == "nuo-server" {
            let mut bad_deps: Vec<&str> = deps
                .intersection(&terminal_substrate)
                .chain(deps.intersection(&presentation_crates))
                .chain(deps.intersection(&retired_crates))
                .copied()
                .collect();
            bad_deps.sort();
            for bad in bad_deps {
                problems.push(format!(
                    "R1: nuo-server ({}) depends on presentation substrate '{bad}' (server purity)",
                    info.rel_path
                ));
            }
        }

        // R2: Retired crates prohibition
        let mut bad_retired: Vec<&str> = deps.intersection(&retired_crates).copied().collect();
        bad_retired.sort();
        for bad in bad_retired {
            problems.push(format!(
                "R2: '{name}' ({}) depends on retired presentation crate '{bad}'",
                info.rel_path
            ));
        }

        // R2: Presentation layer isolation
        if !presentation_layer.contains(name.as_str()) {
            let mut bad_term: Vec<&str> = deps.intersection(&terminal_substrate).copied().collect();
            bad_term.sort();
            for bad in bad_term {
                problems.push(format!(
                    "R2: '{name}' ({}) depends on terminal substrate '{bad}'; only the presentation layer may",
                    info.rel_path
                ));
            }

            let mut bad_pres: Vec<&str> =
                deps.intersection(&presentation_crates).copied().collect();
            bad_pres.sort();
            for bad in bad_pres {
                problems.push(format!(
                    "R2: '{name}' ({}) depends on presentation crate '{bad}'; only the presentation layer or `nuo` host binary may (client-server anti-inversion)",
                    info.rel_path
                ));
            }
        }
    }

    // R3: Check zero-IO on presentation crates
    for target in PRESENTATION_CRATES {
        if let Some(info) = manifests.get(*target) {
            let deps: HashSet<&str> = info.shipped_deps.iter().map(|s| s.as_str()).collect();
            let mut bad_io: Vec<&str> = deps.intersection(&ui_forbidden).copied().collect();
            bad_io.sort();
            for bad in bad_io {
                problems.push(format!(
                    "R3: {target} ({}) declares forbidden IO dependency '{bad}' ([INV-ARCH-UI-ZERO-IO])",
                    info.rel_path
                ));
            }
        } else {
            problems.push(format!(
                "R3: crates/{target} is absent (presentation substrate)"
            ));
        }
    }

    // R4: Check primitives purity
    if let Some(primitives) = manifests.get("nuo-ui-primitives") {
        if primitives.shipped_deps.contains("nuo-view") {
            problems.push(format!(
                "R4: nuo-ui-primitives ({}) depends on domain views 'nuo-view' ([INV-ARCH-UI-PRIMITIVES-PURITY])",
                primitives.rel_path
            ));
        }
    }

    // Retired crates presence in workspace
    for retired in RETIRED_CRATES {
        if manifests.contains_key(*retired) {
            problems.push(format!(
                "R2: crates/{retired} is retired and must not be present in the workspace (superseded by nuo-ui-primitives, nuo-view, and nuo::shell)"
            ));
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

pub fn check_lean_cli_link(root: &Path) -> Result<(), String> {
    let cargo_bin = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = Command::new(cargo_bin)
        .current_dir(root)
        .args(["tree", "-p", "nuo", "--no-default-features", "-e", "normal"])
        .output()
        .map_err(|e| format!("Failed to execute cargo tree: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "cargo tree failed with status {:?}:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let forbidden = [
        "nuotc",
        "crossterm",
        "nuo-ui-primitives",
        "nuo-view",
        "nuo-present",
        "nuo-ui",
        "nuo-shell",
    ];

    let mut matched = BTreeSet::new();
    for line in stdout.lines() {
        for f in &forbidden {
            if line.contains(f) {
                matched.insert(*f);
            }
        }
    }

    if !matched.is_empty() {
        return Err(format!(
            "lean build links a terminal presentation stack (INV-ARCH-CLI-LEAN-LINK): matched forbidden crates: {:?}",
            matched
        ));
    }

    Ok(())
}
