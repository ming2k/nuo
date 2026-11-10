//! Wire-surface census: the protocol number must move whenever the wire does.
//!
//! Rule source: ADR-0136 / `[INV-WIRE-03]`.
//!
//! The census records two facts per wire type: its member names (for a
//! reviewable diff) and a SHA-256 shape digest over the normalized declaration
//! (for the changes a name list cannot see — fields, types, and wire-relevant
//! attributes). A variant-name list alone let ADR-0167 change `StreamDiscard`
//! from a unit variant to a struct variant without moving the protocol number;
//! the shape digest is what makes that class of drift fail.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SurfaceKind {
    Enum,
    Struct,
}

/// The declarations whose shape constitutes the wire surface.
///
/// The nested `RoundScope` and `TurnContext` are listed explicitly: a digest
/// over `AgentResponse` only names the referenced type, so a field change in
/// the nested struct would otherwise be invisible.
const CENSUS_TYPES: &[(&str, &str, SurfaceKind)] = &[
    ("Wire", "crates/nuo-protocol/src/lib.rs", SurfaceKind::Enum),
    (
        "AgentRequest",
        "crates/nuo-protocol/src/events.rs",
        SurfaceKind::Enum,
    ),
    (
        "AgentResponse",
        "crates/nuo-protocol/src/events.rs",
        SurfaceKind::Enum,
    ),
    (
        "RoundEvent",
        "crates/nuo-protocol/src/events.rs",
        SurfaceKind::Enum,
    ),
    (
        "RoundScope",
        "crates/nuo-protocol/src/events.rs",
        SurfaceKind::Enum,
    ),
    (
        "TurnContext",
        "crates/nuo-protocol/src/events.rs",
        SurfaceKind::Struct,
    ),
];

/// Where the recorded census lives, relative to the workspace root.
pub const CENSUS_FILE: &str = "crates/nuo/src/dev/wire-census.txt";

/// One type's recorded surface: its member names plus the structural digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeCensus {
    /// Variant or field names, in source order. Empty for a struct whose
    /// fields are not listed separately; the shape digest covers them.
    pub members: Vec<String>,
    /// SHA-256 over the normalized declaration and its container
    /// `#[serde(...)]` / serde-bearing `#[cfg_attr(...)]` attributes.
    pub shape: String,
}

/// A protocol number plus every wire type's recorded surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Census {
    pub protocol: u32,
    /// Type name → recorded surface.
    pub types: BTreeMap<String, TypeCensus>,
}

impl Census {
    /// Render the file format: a header, then a variant line and a shape line per enum.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "# Wire surface census — regenerate with `cargo test -p nuo --test architecture_firewall -- --ignored bless_wire_census`."
        );
        let _ = writeln!(
            out,
            "# `Type:` lists variant names; `Type@shape:` is SHA-256 over the"
        );
        let _ = writeln!(
            out,
            "# normalized declaration (comments and layout ignored, container serde"
        );
        let _ = writeln!(
            out,
            "# attributes included). Changing a field, type, or wire attribute is a"
        );
        let _ = writeln!(out, "# wire change even when every variant keeps its name.");
        let _ = writeln!(
            out,
            "# Any wire change requires a PROTOCOL_VERSION bump and a renewed census."
        );
        let _ = writeln!(out, "protocol {}", self.protocol);
        for (name, census) in &self.types {
            if !census.members.is_empty() {
                let _ = writeln!(out, "{name}: {}", census.members.join(" "));
            }
            let _ = writeln!(out, "{name}@shape: {}", census.shape);
        }
        out
    }

    /// Parse the file format. Returns `None` when the text is not a census.
    pub fn parse(text: &str) -> Option<Self> {
        let mut protocol = None;
        let mut members: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut shapes: BTreeMap<String, String> = BTreeMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix("protocol ") {
                protocol = rest.trim().parse::<u32>().ok();
                continue;
            }
            if let Some((name, rest)) = line.split_once("@shape:") {
                shapes.insert(name.trim().to_string(), rest.trim().to_string());
                continue;
            }
            let (name, rest) = line.split_once(':')?;
            members.insert(
                name.trim().to_string(),
                rest.split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>(),
            );
        }

        let mut types = BTreeMap::new();
        for (name, members) in members {
            let shape = shapes.remove(&name)?;
            if shape.is_empty() {
                return None;
            }
            types.insert(name, TypeCensus { members, shape });
        }
        // A struct line may be shape-only; consume any remaining shape-only
        // entries as empty-member types.
        for (name, shape) in shapes {
            if shape.is_empty() {
                return None;
            }
            types.insert(
                name,
                TypeCensus {
                    members: Vec::new(),
                    shape,
                },
            );
        }
        Some(Self {
            protocol: protocol?,
            types,
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

/// Hash the normalized declaration of `enum_name` plus its container serde
/// attributes.
///
/// Normalization removes comments, doc comments, and all whitespace outside
/// string/char literals. It therefore ignores rustfmt churn while preserving
/// every token that can change the wire: variant names, field names, types,
/// field/variant attributes, and the container's serde tag/rename policy.
fn shape_of(source: &str, type_name: &str, kind: SurfaceKind) -> Option<String> {
    let keyword = match kind {
        SurfaceKind::Enum => "enum",
        SurfaceKind::Struct => "struct",
    };
    let needle = format!("pub {keyword} {type_name} ");
    let start = source.find(&needle)?;
    let open = start + source[start..].find('{')?;
    let close = matching_brace(source, open)?;

    let mut normalized = String::new();
    for attribute in container_wire_attributes(&source[..start]) {
        normalized.push_str(&canonical_code(&attribute));
    }
    normalized.push_str(&canonical_code(&source[start..=close]));
    Some(sha256_hex(&strip_trailing_commas(&normalized)))
}

/// The contiguous `#[serde(...)]` and serde-bearing `#[cfg_attr(...)]`
/// attributes immediately above a declaration.
///
/// Derive/allow/doc attributes are deliberately excluded: they do not change
/// the wire and must not force a protocol bump.
fn container_wire_attributes(prefix: &str) -> Vec<String> {
    let mut attributes = Vec::new();
    let mut cursor = prefix.len();
    loop {
        cursor = skip_trivia_backwards(prefix, cursor);
        if cursor == 0 || !prefix[..cursor].ends_with(']') {
            break;
        }
        let close = cursor - 1;
        let Some(open) = matching_open_bracket(prefix, close) else {
            break;
        };
        let before_open = prefix[..open].trim_end();
        if !before_open.ends_with('#') {
            break;
        }
        let hash = before_open.len() - 1;
        let attribute = &prefix[hash..=close];
        let trimmed = attribute.trim_start();
        if trimmed.starts_with("#[serde")
            || (trimmed.starts_with("#[cfg_attr") && trimmed.contains("serde"))
        {
            attributes.push(attribute.to_string());
        }
        cursor = hash;
    }
    attributes.reverse();
    attributes
}

/// Skip whitespace and comments immediately before `cursor`.
fn skip_trivia_backwards(text: &str, cursor: usize) -> usize {
    let mut cursor = cursor;
    loop {
        let trimmed = text[..cursor].trim_end();
        cursor = trimmed.len();
        if cursor == 0 {
            return 0;
        }
        let line_start = text[..cursor].rfind('\n').map_or(0, |index| index + 1);
        if text[line_start..cursor].trim_start().starts_with("//") {
            cursor = line_start;
            continue;
        }
        if text[..cursor].ends_with("*/")
            && let Some(open) = text[..cursor - 2].rfind("/*")
        {
            cursor = open;
            continue;
        }
        return cursor;
    }
}

/// Match the `[` that opens the `]` at `close`.
fn matching_open_bracket(text: &str, close: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut index = close + 1;
    while index > 0 {
        index -= 1;
        match bytes[index] {
            b']' => depth += 1,
            b'[' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

/// Match the `}` that closes the `{` at `open`, ignoring braces in comments
/// and literals.
fn matching_brace(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut index = open;
    while index < text.len() {
        let rest = &text[index..];
        if rest.starts_with("//") {
            index = line_comment_end(text, index);
            continue;
        }
        if rest.starts_with("/*") {
            index = block_comment_end(text, index)?;
            continue;
        }
        if let Some(end) = raw_string_end(text, index) {
            index = end;
            continue;
        }
        let character = rest.chars().next()?;
        match character {
            '"' => index = quoted_end(text, index)?,
            '\'' => index = char_or_lifetime_end(text, index),
            '{' => {
                depth += 1;
                index += 1;
            }
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
                index += 1;
            }
            _ => index += character.len_utf8(),
        }
    }
    None
}

/// Remove comments and whitespace outside literals.
fn canonical_code(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        let rest = &text[index..];
        if rest.starts_with("//") {
            index = line_comment_end(text, index);
            continue;
        }
        if rest.starts_with("/*") {
            index = block_comment_end(text, index).unwrap_or(text.len());
            continue;
        }
        if let Some(end) = raw_string_end(text, index) {
            out.push_str(&text[index..end]);
            index = end;
            continue;
        }
        let character = match rest.chars().next() {
            Some(character) => character,
            None => break,
        };
        match character {
            '"' => {
                let end = quoted_end(text, index).unwrap_or(text.len());
                out.push_str(&text[index..end]);
                index = end;
            }
            '\'' => {
                let end = char_or_lifetime_end(text, index);
                out.push_str(&text[index..end]);
                index = end;
            }
            character if character.is_whitespace() => index += character.len_utf8(),
            character => {
                out.push(character);
                index += character.len_utf8();
            }
        }
    }
    out
}

fn line_comment_end(text: &str, start: usize) -> usize {
    text[start..]
        .find('\n')
        .map_or(text.len(), |offset| start + offset + 1)
}

fn block_comment_end(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 1usize;
    let mut index = start + 2;
    while index + 1 < bytes.len() {
        if bytes[index] == b'/' && bytes[index + 1] == b'*' {
            depth += 1;
            index += 2;
        } else if bytes[index] == b'*' && bytes[index + 1] == b'/' {
            depth -= 1;
            index += 2;
            if depth == 0 {
                return Some(index);
            }
        } else {
            index += 1;
        }
    }
    None
}

/// End of a normal quoted string starting at `start`.
fn quoted_end(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'"' => return Some(index + 1),
            _ => index += 1,
        }
    }
    None
}

/// End of a raw string (`r"..."`, `r#"..."#`, `br#"..."#`, `cr#"..."#`) at
/// `start`, or `None` when `start` is not a raw-string prefix.
fn raw_string_end(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut index = start;
    match bytes.get(index) {
        Some(b'r') => index += 1,
        Some(b'b' | b'c') if bytes.get(index + 1) == Some(&b'r') => index += 2,
        _ => return None,
    }
    let mut hashes = 0usize;
    while bytes.get(index) == Some(&b'#') {
        hashes += 1;
        index += 1;
    }
    if bytes.get(index) != Some(&b'"') {
        return None;
    }
    index += 1;
    while index < bytes.len() {
        if bytes[index] == b'"' {
            let mut matched = 0usize;
            while matched < hashes && bytes.get(index + 1 + matched) == Some(&b'#') {
                matched += 1;
            }
            if matched == hashes {
                return Some(index + 1 + hashes);
            }
        }
        index += 1;
    }
    Some(bytes.len())
}

/// End of a char literal at `start`, or just past the apostrophe for a lifetime.
fn char_or_lifetime_end(text: &str, start: usize) -> usize {
    let bytes = text.as_bytes();
    let mut index = start + 1;
    if bytes.get(index) == Some(&b'\\') {
        index += 2;
    } else {
        index += 1;
    }
    if bytes.get(index) == Some(&b'\'') {
        index + 1
    } else {
        start + 1
    }
}

/// Drop commas that only trail a field, parameter, or argument list.
///
/// Rustfmt adds and removes them by layout, and they never change the wire.
fn strip_trailing_commas(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character == ',' && matches!(characters.peek(), Some('}' | ')' | ']')) {
            continue;
        }
        out.push(character);
    }
    out
}

fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Recompute the census from the tree as it stands.
pub fn compute_census(root: &Path) -> Result<Census, String> {
    let mut types = BTreeMap::new();
    let mut cache: BTreeMap<&str, String> = BTreeMap::new();
    for (type_name, rel_path, kind) in CENSUS_TYPES {
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
        let members = match kind {
            SurfaceKind::Enum => {
                let variants = variants_of(&source, type_name)
                    .ok_or_else(|| format!("enum `{type_name}` not found in {rel_path}"))?;
                if variants.is_empty() {
                    return Err(format!(
                        "enum `{type_name}` in {rel_path} parsed to zero variants; the census \
                         parser no longer matches how the enum is written"
                    ));
                }
                variants
            }
            SurfaceKind::Struct => Vec::new(),
        };
        let shape = shape_of(&source, type_name, *kind)
            .ok_or_else(|| format!("type `{type_name}` in {rel_path} has no shape"))?;
        types.insert(type_name.to_string(), TypeCensus { members, shape });
    }
    Ok(Census {
        protocol: nuo_protocol::PROTOCOL_VERSION,
        types,
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
        if recorded.types == current.types {
            return Ok(());
        }
        return Err(format!(
            "wire surface changed without a protocol bump (both at protocol {}):\n{}\
             \nA peer still speaking protocol {} cannot decode these frames.\n\
             Bump `PROTOCOL_VERSION` in crates/nuo-protocol/src/lib.rs, then renew the census in the same commit.",
            current.protocol,
            describe_changes(&recorded, &current),
            current.protocol,
        ));
    }

    if recorded.types == current.types {
        return Err(format!(
            "PROTOCOL_VERSION advanced {} -> {} but the wire surface is byte-for-byte\n\
             unchanged. Either the bump was accidental, or a change was recorded under\n\
             the old number and then edited back.",
            recorded.protocol, current.protocol
        ));
    }

    Ok(())
}

fn describe_changes(recorded: &Census, current: &Census) -> String {
    let mut report = String::new();
    for (name, current_type) in &current.types {
        let Some(recorded_type) = recorded.types.get(name) else {
            let _ = writeln!(report, "  {name}: type added");
            continue;
        };
        let added: Vec<&String> = current_type
            .members
            .iter()
            .filter(|member| !recorded_type.members.contains(member))
            .collect();
        let removed: Vec<&String> = recorded_type
            .members
            .iter()
            .filter(|member| !current_type.members.contains(member))
            .collect();
        if !added.is_empty() {
            let _ = writeln!(
                report,
                "  {name}:\n    added:   {}",
                added
                    .iter()
                    .map(|variant| variant.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
        if !removed.is_empty() {
            let _ = writeln!(
                report,
                "  {name}:\n    removed: {}",
                removed
                    .iter()
                    .map(|variant| variant.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
        if recorded_type.shape != current_type.shape {
            let _ = writeln!(
                report,
                "  {name}: shape changed\n    recorded: {}\n    current:  {}",
                recorded_type.shape, current_type.shape
            );
        }
    }
    for name in recorded.types.keys() {
        if !current.types.contains_key(name) {
            let _ = writeln!(report, "  {name}: type removed");
        }
    }
    report
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
    fn shape_ignores_layout_and_comments_but_not_fields() {
        let first = r#"
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[allow(clippy::large_enum_variant)]
pub enum Wire {
    /// A doc comment that changes nothing.
    Select {
        action: u8,
        project: Option<u8>,
    },
    Exit,
}
"#;
        let second = r#"#[serde(tag="type")] pub enum Wire { Select { action: u8, project: Option<u8> }, Exit }"#;
        assert_eq!(
            shape_of(first, "Wire", SurfaceKind::Enum),
            shape_of(second, "Wire", SurfaceKind::Enum),
            "comments, derives and layout must not enter the shape digest"
        );

        let third = first.replace("action: u8", "action: u16");
        assert_ne!(
            shape_of(first, "Wire", SurfaceKind::Enum),
            shape_of(&third, "Wire", SurfaceKind::Enum),
            "a field type is a wire change"
        );

        let fourth = first.replace("tag = \"type\"", "tag = \"kind\"");
        assert_ne!(
            shape_of(first, "Wire", SurfaceKind::Enum),
            shape_of(&fourth, "Wire", SurfaceKind::Enum),
            "the container serde tag is a wire change"
        );
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
    fn a_shape_change_without_a_bump_is_rejected() {
        let root = temp_root("shape_without_bump");
        write_enum(&root, "Wire", "pub enum Wire {\n    Select { a: u8 },\n}\n");
        write_events(
            &root,
            "pub enum AgentRequest {\n    Prompt,\n}\n\
             pub enum AgentResponse {\n    Round,\n}\n\
             pub enum RoundEvent {\n    StreamDiscard,\n}\n",
        );
        bless_census(&root).unwrap();

        write_events(
            &root,
            "pub enum AgentRequest {\n    Prompt,\n}\n\
             pub enum AgentResponse {\n    Round,\n}\n\
             pub enum RoundEvent {\n    StreamDiscard { round: u64, turn: u64 },\n}\n",
        );
        let err = check_wire_census(&root).unwrap_err();
        assert!(
            err.contains("without a protocol bump"),
            "must name the defect: {err}"
        );
        assert!(
            err.contains("RoundEvent: shape changed"),
            "must name the enum whose shape moved: {err}"
        );
    }

    #[test]
    fn a_nested_struct_shape_change_without_a_bump_is_rejected() {
        let root = temp_root("nested_shape_without_bump");
        write_enum(&root, "Wire", "pub enum Wire {\n    Select { a: u8 },\n}\n");
        write_events(
            &root,
            "pub enum AgentRequest {\n    Prompt,\n}\n\
             pub enum AgentResponse {\n    Round,\n}\n\
             pub enum RoundEvent {\n    Text,\n}\n",
        );
        bless_census(&root).unwrap();

        let path = root.join("crates/nuo-protocol/src/events.rs");
        let source = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, source.replace("pub turn: u64", "pub turn: u32")).unwrap();
        let err = check_wire_census(&root).unwrap_err();
        assert!(
            err.contains("TurnContext: shape changed"),
            "must see a change inside the nested context struct: {err}"
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

    #[test]
    fn a_bump_with_a_shape_change_is_accepted() {
        let root = temp_root("bump_with_shape_change");
        write_enum(&root, "Wire", "pub enum Wire {\n    Select { a: u8 },\n}\n");
        write_events(
            &root,
            "pub enum AgentRequest {\n    Prompt,\n}\n\
             pub enum AgentResponse {\n    Round,\n}\n\
             pub enum RoundEvent {\n    StreamDiscard,\n}\n",
        );
        let blessed = bless_census(&root).unwrap();
        let current = nuo_protocol::PROTOCOL_VERSION;
        let previous = current - 1;
        let text = std::fs::read_to_string(&blessed).unwrap().replace(
            &format!("protocol {current}"),
            &format!("protocol {previous}"),
        );
        std::fs::write(&blessed, text).unwrap();

        write_events(
            &root,
            "pub enum AgentRequest {\n    Prompt,\n}\n\
             pub enum AgentResponse {\n    Round,\n}\n\
             pub enum RoundEvent {\n    StreamDiscard { round: u64, turn: u64 },\n}\n",
        );
        check_wire_census(&root)
            .expect("a bumped protocol with a changed shape is a valid wire move");
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
        let mut source = body.to_string();
        if !source.contains("pub struct TurnContext") {
            source.push_str(
                "\npub struct TurnContext {\n    pub round: u64,\n    pub turn: u64,\n    pub sent_at_ms: u64,\n}\n",
            );
        }
        if !source.contains("pub enum RoundScope") {
            source.push_str(
                "\npub enum RoundScope {\n    Turn { context: TurnContext, event: RoundEvent },\n    Session { event: RoundEvent },\n}\n",
            );
        }
        std::fs::write(root.join("crates/nuo-protocol/src/events.rs"), source).unwrap();
    }
}
