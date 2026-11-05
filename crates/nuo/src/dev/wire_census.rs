//! Wire-surface census: the protocol number must move whenever the wire does.
//!
//! Rule source: ADR-0136 / `[INV-WIRE-03]`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The enums whose variant sets constitute the wire surface.
const CENSUS_ENUMS: &[(&str, &str)] = &[
    ("Wire", "crates/nuo-protocol/src/lib.rs"),
    ("AgentRequest", "crates/nuo-protocol/src/events.rs"),
    ("AgentResponse", "crates/nuo-protocol/src/events.rs"),
    ("RoundEvent", "crates/nuo-protocol/src/events.rs"),
];

/// Where the recorded census lives, relative to the workspace root.
pub const CENSUS_FILE: &str = "crates/nuo/src/dev/wire-census.txt";

/// A protocol number plus every enum's variant names, in declaration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Census {
    pub protocol: u32,
    /// Enum name → variant names, in source order.
    pub enums: BTreeMap<String, Vec<String>>,
}

impl Census {
    /// Render the file format: a header line, then one `Enum: A B C` per enum.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "# Wire surface census — regenerate with `nuo dev census --bless`."
        );
        let _ = writeln!(
            out,
            "# Every line below is a variant the peer must model to speak this protocol."
        );
        let _ = writeln!(
            out,
            "# Changing any of it requires a PROTOCOL_VERSION bump."
        );
        let _ = writeln!(out, "protocol {}", self.protocol);
        for (name, variants) in &self.enums {
            let _ = writeln!(out, "{name}: {}", variants.join(" "));
        }
        out
    }

    /// Parse the file format. Returns `None` when the text is not a census.
    pub fn parse(text: &str) -> Option<Self> {
        let mut protocol = None;
        let mut enums = BTreeMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix("protocol ") {
                protocol = rest.trim().parse::<u32>().ok();
                continue;
            }
            let (name, rest) = line.split_once(':')?;
            let variants = rest
                .split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>();
            enums.insert(name.trim().to_string(), variants);
        }
        Some(Self {
            protocol: protocol?,
            enums,
        })
    }
}

/// Extract the variant names of `enum_name` from `source`.
fn variants_of(source: &str, enum_name: &str) -> Option<Vec<String>> {
    let needle = format!("pub enum {enum_name} ");
    let start = source.find(&needle)?;
    let rest = &source[start..];
    let open = rest.find('{')?;

    let mut depth = 0usize;
    let mut end = None;
    for (offset, ch) in rest[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(open + offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let body = &rest[open..=end?];

    let mut variants = Vec::new();
    let mut depth = 0usize;
    for line in body.lines().skip(1) {
        let trimmed = line.trim_start();
        if depth == 0
            && let Some(name) = variant_name(trimmed)
        {
            variants.push(name);
        }
        depth = depth
            .saturating_add(line.matches('{').count())
            .saturating_sub(line.matches('}').count());
    }
    Some(variants)
}

/// The variant name a line declares, if it declares one.
fn variant_name(trimmed: &str) -> Option<String> {
    let first = trimmed.chars().next()?;
    if !first.is_ascii_uppercase() {
        return None;
    }
    let ident: String = trimmed
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    let rest = &trimmed[ident.len()..];
    if rest.trim_start().starts_with(':') && !rest.trim_start().starts_with("::") {
        return None;
    }
    Some(ident)
}

/// Recompute the census from the tree as it stands.
pub fn compute_census(root: &Path) -> Result<Census, String> {
    let mut enums = BTreeMap::new();
    let mut cache: BTreeMap<&str, String> = BTreeMap::new();
    for (enum_name, rel_path) in CENSUS_ENUMS {
        let source = match cache.get(rel_path) {
            Some(source) => source.clone(),
            None => {
                let path = root.join(rel_path);
                let source = std::fs::read_to_string(&path)
                    .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
                cache.insert(rel_path, source.clone());
                source
            }
        };
        let variants = variants_of(&source, enum_name)
            .ok_or_else(|| format!("enum `{enum_name}` not found in {rel_path}"))?;
        if variants.is_empty() {
            return Err(format!(
                "enum `{enum_name}` in {rel_path} parsed to zero variants; the census \
                 parser no longer matches how the enum is written"
            ));
        }
        enums.insert(enum_name.to_string(), variants);
    }
    Ok(Census {
        protocol: nuo_protocol::PROTOCOL_VERSION,
        enums,
    })
}

/// The recorded census, or `None` when the file does not exist yet.
pub fn recorded_census(root: &Path) -> Option<Census> {
    let text = std::fs::read_to_string(root.join(CENSUS_FILE)).ok()?;
    Census::parse(&text)
}

/// Rewrite the recorded census from the tree. Run after bumping the protocol.
pub fn bless_census(root: &Path) -> Result<PathBuf, String> {
    let census = compute_census(root)?;
    let path = root.join(CENSUS_FILE);
    std::fs::write(&path, census.render())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path)
}

/// `[INV-WIRE-03]`: the wire census and the protocol number agree.
pub fn check_wire_census(root: &Path) -> Result<(), String> {
    let current = compute_census(root)?;
    let Some(recorded) = recorded_census(root) else {
        return Err(format!("no wire census at {CENSUS_FILE}."));
    };

    if recorded.protocol == current.protocol {
        if recorded.enums == current.enums {
            return Ok(());
        }
        let mut report = String::new();
        let _ = writeln!(
            report,
            "wire surface changed without a protocol bump (both at protocol {}):",
            current.protocol
        );
        let empty = Vec::new();
        for (name, variants) in &current.enums {
            let before = recorded.enums.get(name).unwrap_or(&empty);
            let added: Vec<&String> = variants.iter().filter(|v| !before.contains(v)).collect();
            let removed: Vec<&String> = before.iter().filter(|v| !variants.contains(v)).collect();
            if added.is_empty() && removed.is_empty() {
                continue;
            }
            let _ = writeln!(report, "  {name}:");
            if !added.is_empty() {
                let _ = writeln!(
                    report,
                    "    added:   {}",
                    added
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }
            if !removed.is_empty() {
                let _ = writeln!(
                    report,
                    "    removed: {}",
                    removed
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }
        }
        let _ = writeln!(
            report,
            "\nA peer still speaking protocol {} cannot decode these frames.\n\
             Bump `PROTOCOL_VERSION` in crates/nuo-protocol/src/lib.rs, then renew the census in the same commit.",
            current.protocol
        );
        return Err(report);
    }

    if recorded.enums == current.enums {
        return Err(format!(
            "PROTOCOL_VERSION advanced {} -> {} but the wire surface is byte-for-byte\n\
             unchanged. Either the bump was accidental, or a change was recorded under\n\
             the old number and then edited back.",
            recorded.protocol, current.protocol
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_are_read_through_struct_bodies_and_doc_comments() {
        let source = r#"
pub enum Wire {
    /// A doc comment that changes nothing.
    Select {
        action: u8,
        project: Option<u8>,
    },
    Welcome { thread_id: String },
    Exit,
    Error {
        message: String,
        code: Option<String>,
    },
}
"#;
        let variants = variants_of(source, "Wire").unwrap();
        assert_eq!(variants, vec!["Select", "Welcome", "Exit", "Error"]);
    }

    #[test]
    fn a_removed_variant_without_a_bump_is_rejected() {
        let root = temp_root("removed_without_bump");
        write_enum(
            &root,
            "Wire",
            "pub enum Wire {\n    Select { a: u8 },\n    Welcome { b: u8 },\n}\n",
        );
        write_events(
            &root,
            "pub enum AgentRequest {\n    Prompt,\n}\n\
             pub enum AgentResponse {\n    Round,\n    Exit,\n}\n\
             pub enum RoundEvent {\n    Text,\n}\n",
        );
        let blessed = bless_census(&root).unwrap();
        let text = std::fs::read_to_string(&blessed).unwrap();
        assert!(
            text.contains(&format!("protocol {}", nuo_protocol::PROTOCOL_VERSION)),
            "{text}"
        );

        write_events(
            &root,
            "pub enum AgentRequest {\n    Prompt,\n}\n\
             pub enum AgentResponse {\n    Round,\n}\n\
             pub enum RoundEvent {\n    Text,\n}\n",
        );
        let err = check_wire_census(&root).unwrap_err();
        assert!(
            err.contains("without a protocol bump"),
            "must name the defect: {err}"
        );
        assert!(
            err.contains("removed: Exit"),
            "must name the variant that vanished: {err}"
        );
    }

    #[test]
    fn a_bump_with_an_unchanged_surface_is_rejected() {
        let root = temp_root("bump_without_change");
        write_enum(&root, "Wire", "pub enum Wire {\n    Select { a: u8 },\n}\n");
        write_events(
            &root,
            "pub enum AgentRequest {\n    Prompt,\n}\n\
             pub enum AgentResponse {\n    Round,\n}\n\
             pub enum RoundEvent {\n    Text,\n}\n",
        );
        let blessed = bless_census(&root).unwrap();
        let current = nuo_protocol::PROTOCOL_VERSION;
        let previous = current - 1;
        let text = std::fs::read_to_string(&blessed).unwrap().replace(
            &format!("protocol {current}"),
            &format!("protocol {previous}"),
        );
        std::fs::write(&blessed, text).unwrap();

        let err = check_wire_census(&root).unwrap_err();
        assert!(
            err.contains(&format!("advanced {previous} -> {current}")) && err.contains("unchanged"),
            "must report a bump with nothing behind it: {err}"
        );
    }

    fn temp_root(label: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("nuo-wire-census-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("crates/nuo-protocol/src")).unwrap();
        std::fs::create_dir_all(root.join("crates/nuo/src/dev")).unwrap();
        root
    }

    fn write_enum(root: &Path, name: &str, body: &str) {
        assert_eq!(name, "Wire");
        std::fs::write(root.join("crates/nuo-protocol/src/lib.rs"), body).unwrap();
    }

    fn write_events(root: &Path, body: &str) {
        std::fs::write(root.join("crates/nuo-protocol/src/events.rs"), body).unwrap();
    }
}
