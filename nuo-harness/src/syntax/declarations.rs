//! Named-declaration extraction: the structural index behind structural
//! queries (ADR-0237).
//!
//! `extract_symbols` answers "what does this file look like?"
//! by rendering one signature line per top-level item. A structural *query*
//! needs something stronger: a stable **kind**, a **name** to match on, and a
//! **byte range** to slice source from. That is what this module produces.
//!
//! Two deliberate limits keep the result honest (ADR-0214: structure output is
//! bounded evidence, never semantic ground truth):
//!
//! - **Kinds are a closed vocabulary** ([`DECLARATION_KINDS`]), mapped from
//!   each grammar's node kinds. The model never writes a tree-sitter query, so
//!   a miss is a miss against a small word list rather than a broken
//!   S-expression.
//! - **Extraction is bounded** by `MAX_DECLARATIONS` and
//!   `MAX_NESTING_DEPTH`. It is a syntactic index, not a type-aware symbol
//!   table: no resolution, no generics, no macro expansion.

use tree_sitter::Node;

use super::SupportedLanguage;

/// The closed vocabulary of declaration kinds a structural query may name.
///
/// `method` is a `fn` (or `func`) declared inside a container; `impl` names the
/// implemented type. A kind outside this list is an error naming the list, not
/// a silently empty result.
pub const DECLARATION_KINDS: &[&str] = &[
    "fn",
    "method",
    "struct",
    "enum",
    "trait",
    "impl",
    "class",
    "interface",
    "type",
    "const",
    "static",
    "mod",
    "macro",
];

/// How many declarations one file may contribute before extraction stops.
/// Bounds the work a pathological file can demand from a single query.
const MAX_DECLARATIONS: usize = 5_000;

/// How deep container recursion goes below the root. Three levels covers
/// `mod` → `impl` → `fn` and `class` → `method` without walking whole bodies.
const MAX_NESTING_DEPTH: usize = 3;

/// One named declaration found in parsed source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    /// Canonical kind from [`DECLARATION_KINDS`].
    pub kind: &'static str,
    /// The declaration's own (leaf) name, e.g. `handle` for `Foo::handle`.
    pub name: String,
    /// Enclosing container names, outermost first, e.g. `["Foo"]` for a method
    /// declared in `impl Foo`. Empty for a top-level declaration.
    pub containers: Vec<String>,
    /// 1-based first line of the declaration.
    pub start_line: usize,
    /// 1-based last line of the declaration.
    pub end_line: usize,
    /// Byte range of the declaration in its file — the slice a symbol query
    /// returns as source.
    pub byte_range: (usize, usize),
    /// First source line, trimmed — the same rendering an outline shows, so a
    /// query result and an outline agree on how an item reads.
    pub signature: String,
}

impl Declaration {
    /// The fully qualified name, e.g. `Foo::handle`. Used for display and for
    /// qualified symbol lookup.
    pub fn qualified_name(&self) -> String {
        if self.containers.is_empty() {
            self.name.clone()
        } else {
            format!("{}::{}", self.containers.join("::"), self.name)
        }
    }
}

/// Extract every named declaration from `content`, bounded in both depth and
/// count. Unsupported languages, unparseable input, and anonymous items yield
/// an empty or shorter list — never a guess.
pub fn extract_declarations(ext: &str, content: &str) -> Vec<Declaration> {
    let Some(language) = SupportedLanguage::from_extension(ext) else {
        return Vec::new();
    };
    let mut parser = tree_sitter::Parser::new();
    if parser
        .set_language(&language.tree_sitter_language())
        .is_err()
    {
        return Vec::new();
    }
    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };

    let mut found = Vec::new();
    collect(
        language,
        tree.root_node(),
        content,
        &mut Vec::new(),
        0,
        &mut found,
    );
    found.truncate(MAX_DECLARATIONS);
    found
}

/// Depth-first walk of container nodes, emitting a [`Declaration`] per named
/// item and recursing into containers.
fn collect(
    language: SupportedLanguage,
    parent: Node<'_>,
    source: &str,
    containers: &mut Vec<String>,
    depth: usize,
    found: &mut Vec<Declaration>,
) {
    if found.len() >= MAX_DECLARATIONS {
        return;
    }
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        if found.len() >= MAX_DECLARATIONS {
            return;
        }
        let node = unwrap_wrapper(language, child);

        if let Some((kind, name)) = classify(language, node, source, depth) {
            // Identity (kind, name) comes from the unwrapped node, but the
            // **span and rendering come from the wrapper**: a decorator
            // (Python) or an `export` keyword (TypeScript) belongs to the
            // declaration, so an outline still reads `export function boot()`
            // and a symbol slice includes the decorator above the `def`.
            let range = child.byte_range();
            let start = child.start_position().row + 1;
            let end = child.end_position().row + 1;
            found.push(Declaration {
                kind,
                name: name.clone(),
                containers: containers.clone(),
                start_line: start,
                end_line: end,
                byte_range: (range.start, range.end.min(source.len())),
                signature: first_line_signature(child, source),
            });

            // A container's *members* are indexable one level in, so
            // `method` and nested `fn` are reachable without walking bodies.
            // The recursion enters the container's **body** node, not the
            // container itself: the members live inside `declaration_list`
            // (Rust), `class_body` (TypeScript), `block` (Python),
            // `field_declaration_list` (C/C++), etc.
            if is_container(language, node) && depth < MAX_NESTING_DEPTH {
                containers.push(name);
                let body = node.child_by_field_name("body").unwrap_or(node);
                collect(language, body, source, containers, depth + 1, found);
                containers.pop();
            }
        } else if is_transparent(language, node) {
            // E.g. a `decorated_definition` (Python) or an `export_statement`
            // (TypeScript) wraps the real declaration at the same depth.
            collect(language, node, source, containers, depth, found);
        }
    }
}

/// Descend through wrapper nodes that do not change the declaration's depth or
/// identity (Python decorators, TypeScript/JavaScript export statements).
fn unwrap_wrapper<'tree>(language: SupportedLanguage, node: Node<'tree>) -> Node<'tree> {
    let mut node = node;
    for _ in 0..4 {
        let inner = match language {
            SupportedLanguage::TypeScript | SupportedLanguage::Tsx => match node.kind() {
                "export_statement" => node.child_by_field_name("declaration"),
                _ => None,
            },
            SupportedLanguage::Python => match node.kind() {
                "decorated_definition" => node.child_by_field_name("definition"),
                _ => None,
            },
            _ => None,
        };
        match inner {
            Some(next) => node = next,
            None => break,
        }
    }
    node
}

/// Wrapper node kinds that carry a declaration one level down.
fn is_transparent(language: SupportedLanguage, node: Node<'_>) -> bool {
    match language {
        SupportedLanguage::Go => node.kind() == "type_declaration",
        _ => false,
    }
}

/// Node kinds whose children are treated as member declarations.
fn is_container(language: SupportedLanguage, node: Node<'_>) -> bool {
    match language {
        SupportedLanguage::Rust => matches!(node.kind(), "impl_item" | "trait_item" | "mod_item"),
        SupportedLanguage::TypeScript | SupportedLanguage::Tsx => matches!(
            node.kind(),
            "class_declaration"
                | "abstract_class_declaration"
                | "interface_declaration"
                | "internal_module"
                | "module"
        ),
        SupportedLanguage::Python => node.kind() == "class_definition",
        SupportedLanguage::C | SupportedLanguage::Cpp => matches!(
            node.kind(),
            "class_specifier" | "namespace_definition" | "struct_specifier"
        ),
        // Go has no nested declarations: methods are top-level nodes.
        SupportedLanguage::Go => false,
    }
}

/// Map a node to its `(kind, name)` pair, or `None` when it is not a named
/// declaration in the vocabulary.
fn classify(
    language: SupportedLanguage,
    node: Node<'_>,
    source: &str,
    depth: usize,
) -> Option<(&'static str, String)> {
    let nested = depth > 0;
    let kind = match language {
        SupportedLanguage::Rust => match node.kind() {
            // `function_signature_item` is a trait method declared without a
            // body (`fn run(&self);`) — a declaration the surface must not
            // silently lose, since trait definitions are a common target.
            "function_item" | "function_signature_item" => {
                if nested {
                    "method"
                } else {
                    "fn"
                }
            }
            "struct_item" => "struct",
            "enum_item" => "enum",
            "trait_item" => "trait",
            "impl_item" => "impl",
            "type_item" => "type",
            "const_item" => "const",
            "static_item" => "static",
            "mod_item" => "mod",
            "macro_definition" => "macro",
            _ => return None,
        },
        SupportedLanguage::TypeScript | SupportedLanguage::Tsx => match node.kind() {
            "function_declaration" | "generator_function_declaration" => {
                if nested {
                    "method"
                } else {
                    "fn"
                }
            }
            "method_definition" | "abstract_method_signature" | "method_signature" => "method",
            "class_declaration" | "abstract_class_declaration" => "class",
            "interface_declaration" => "interface",
            "type_alias_declaration" => "type",
            "enum_declaration" => "enum",
            "internal_module" | "module" => "mod",
            "lexical_declaration" | "variable_declaration" => {
                if !source[node.byte_range()].trim_start().starts_with("const") {
                    return None;
                }
                "const"
            }
            _ => return None,
        },
        SupportedLanguage::Python => match node.kind() {
            "function_definition" => {
                if nested {
                    "method"
                } else {
                    "fn"
                }
            }
            "class_definition" => "class",
            _ => return None,
        },
        SupportedLanguage::C | SupportedLanguage::Cpp => match node.kind() {
            "function_definition" => "fn",
            "struct_specifier" => "struct",
            "class_specifier" => "class",
            "enum_specifier" => "enum",
            "type_definition" => "type",
            "namespace_definition" => "mod",
            _ => return None,
        },
        SupportedLanguage::Go => match node.kind() {
            "function_declaration" => "fn",
            "method_declaration" => "method",
            "type_spec" => match node.child_by_field_name("type").map(|t| t.kind()) {
                Some("struct_type") => "struct",
                Some("interface_type") => "interface",
                _ => "type",
            },
            "const_declaration" => "const",
            _ => return None,
        },
    };
    let name = declaration_name(language, node, source)?;
    Some((kind, name))
}

/// The declaration's own name. Prefers the grammar's `name` field (the common
/// case) and falls back to a bounded search for a first identifier-like child,
/// so a forward declaration or a declarator-wrapped name still resolves.
fn declaration_name(language: SupportedLanguage, node: Node<'_>, source: &str) -> Option<String> {
    if let Some(name) = node.child_by_field_name("name")
        && let Some(text) = node_text(name, source)
        && !text.is_empty()
    {
        return Some(text);
    }
    // `impl Display for Foo` is identified by the type it is implemented for.
    if node.kind() == "impl_item"
        && let Some(target) = node.child_by_field_name("type")
        && let Some(text) = node_text(target, source)
    {
        return Some(text);
    }
    // `impl Foo { … }` (no trait) puts the type in `type` too; covered above.
    // A variable declaration's name lives on its declarator.
    if matches!(node.kind(), "lexical_declaration" | "variable_declaration")
        && let Some(declarator) = find_child_kind(node, "variable_declarator", 3)
        && let Some(name) = declarator.child_by_field_name("name")
        && let Some(text) = node_text(name, source)
    {
        return Some(text);
    }
    // Go wraps a named type in a `type_spec`; `type_spec` has a `name` field
    // (handled above), but a `type_declaration` reaches here when it is
    // transparent, so unwrap once more.
    if node.kind() == "type_declaration"
        && let Some(spec) = find_child_kind(node, "type_spec", 1)
    {
        return declaration_name(language, spec, source);
    }
    // Last resort: the first identifier-like token in the node, bounded.
    let fallback = find_first_identifier(node, 0)?;
    let text = node_text(fallback, source)?;
    let _ = language;
    (!text.is_empty()).then_some(text)
}

/// Breadth-first search for a child (or descendant) of the given kind.
fn find_child_kind<'tree>(node: Node<'tree>, kind: &str, max_depth: usize) -> Option<Node<'tree>> {
    let mut frontier = vec![(node, 0usize)];
    while let Some((current, depth)) = frontier.pop() {
        if depth > max_depth {
            continue;
        }
        let mut cursor = current.walk();
        for child in current.children(&mut cursor) {
            if child.kind() == kind {
                return Some(child);
            }
            frontier.push((child, depth + 1));
        }
    }
    None
}

/// First identifier-like descendant, used only as a name fallback.
fn find_first_identifier(node: Node<'_>, depth: usize) -> Option<Node<'_>> {
    if depth > 6 {
        return None;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if matches!(
            child.kind(),
            "identifier" | "type_identifier" | "field_identifier" | "property_identifier"
        ) {
            return Some(child);
        }
        if let Some(found) = find_first_identifier(child, depth + 1) {
            return Some(found);
        }
    }
    None
}

/// First source line of a node, trimmed of the block/body opener or statement
/// terminator so an outline reads as a signature rather than as the first
/// fragment of an implementation. An inline body (`pub struct User { id: u64 }`)
/// is kept: it is literally how the declaration reads.
fn first_line_signature(node: Node<'_>, source: &str) -> String {
    let text = node_text(node, source).unwrap_or_default();
    let mut line = text.lines().next().unwrap_or("").trim().to_string();
    for suffix in ["{}", "{", ":", ";"] {
        if let Some(stripped) = line.strip_suffix(suffix) {
            line = stripped.trim_end().to_string();
        }
    }
    line
}

/// Node text, or `None` when the range is out of bounds.
fn node_text(node: Node<'_>, source: &str) -> Option<String> {
    let range = node.byte_range();
    if range.end <= source.len() {
        Some(source[range.start..range.end].trim().to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(ext: &str, source: &str) -> Vec<(String, String)> {
        extract_declarations(ext, source)
            .into_iter()
            .map(|decl| (decl.kind.to_string(), decl.qualified_name()))
            .collect()
    }

    #[test]
    fn extracts_rust_items_with_kinds_and_names() {
        let source = "\
pub struct Service;
pub trait Runnable {
    fn run(&self);
}
impl Runnable for Service {
    fn run(&self) {}
}
pub fn top_level() {}
const LIMIT: usize = 3;
";
        let found = names("rs", source);
        assert!(
            found.contains(&("struct".into(), "Service".into())),
            "{found:?}"
        );
        assert!(
            found.contains(&("trait".into(), "Runnable".into())),
            "{found:?}"
        );
        assert!(
            found.contains(&("impl".into(), "Service".into())),
            "{found:?}"
        );
        assert!(
            found.contains(&("fn".into(), "top_level".into())),
            "{found:?}"
        );
        assert!(
            found.contains(&("const".into(), "LIMIT".into())),
            "{found:?}"
        );
        // Members are reachable through their container.
        assert!(
            found.contains(&("method".into(), "Runnable::run".into())),
            "{found:?}"
        );
        assert!(
            found.contains(&("method".into(), "Service::run".into())),
            "{found:?}"
        );
    }

    #[test]
    fn extracts_typescript_through_export_wrappers() {
        let source = "\
export function boot() {}
export class Engine {
  start(): void {}
}
interface Port { open(): void }
const RETRIES = 3;
";
        let found = names("ts", source);
        assert!(found.contains(&("fn".into(), "boot".into())), "{found:?}");
        assert!(
            found.contains(&("class".into(), "Engine".into())),
            "{found:?}"
        );
        assert!(
            found.contains(&("method".into(), "Engine::start".into())),
            "{found:?}"
        );
        assert!(
            found.contains(&("interface".into(), "Port".into())),
            "{found:?}"
        );
        assert!(
            found.contains(&("const".into(), "RETRIES".into())),
            "{found:?}"
        );
    }

    #[test]
    fn extracts_python_through_decorators() {
        let source = "\
class Greeter:
    def greet(self):
        return 1

@decorator
def decorated():
    return 2
";
        let found = names("py", source);
        assert!(
            found.contains(&("class".into(), "Greeter".into())),
            "{found:?}"
        );
        assert!(
            found.contains(&("method".into(), "Greeter::greet".into())),
            "{found:?}"
        );
        assert!(
            found.contains(&("fn".into(), "decorated".into())),
            "{found:?}"
        );
    }

    #[test]
    fn extracts_go_and_c_families() {
        let go =
            "package main\nfunc main() {}\nfunc (s *Server) Start() {}\ntype Config struct{}\n";
        let go_found = names("go", go);
        assert!(
            go_found.contains(&("fn".into(), "main".into())),
            "{go_found:?}"
        );
        assert!(
            go_found.contains(&("method".into(), "Start".into())),
            "{go_found:?}"
        );
        assert!(
            go_found.contains(&("struct".into(), "Config".into())),
            "{go_found:?}"
        );

        let c = "struct Point { int x; };\nint main(void) { return 0; }\n";
        let c_found = names("c", c);
        assert!(
            c_found.contains(&("struct".into(), "Point".into())),
            "{c_found:?}"
        );
        assert!(
            c_found.contains(&("fn".into(), "main".into())),
            "{c_found:?}"
        );
    }

    #[test]
    fn reports_line_and_byte_ranges_that_slice_the_declaration() {
        let source = "pub struct A;\n\npub fn slice_me() {\n    let x = 1;\n}\n";
        let found = extract_declarations("rs", source);
        let target = found
            .iter()
            .find(|decl| decl.name == "slice_me")
            .expect("fn found");
        assert_eq!(target.start_line, 3);
        assert_eq!(target.end_line, 5);
        let (start, end) = target.byte_range;
        assert_eq!(
            &source[start..end],
            "pub fn slice_me() {\n    let x = 1;\n}"
        );
    }

    #[test]
    fn signature_strips_body_openers_and_terminators() {
        let source = "pub struct User;\npub fn run() {}\npub struct Body { id: u64 }\n";
        let declarations = extract_declarations("rs", source);
        assert_eq!(declarations[0].signature, "pub struct User");
        assert_eq!(declarations[1].signature, "pub fn run()");
        assert_eq!(
            declarations[2].signature, "pub struct Body { id: u64 }",
            "an inline body is how the declaration reads; keep it"
        );

        let python = "def handler():\n    pass\n";
        assert_eq!(
            extract_declarations("py", python)[0].signature,
            "def handler()"
        );
    }

    /// A wrapper node is unwrapped for *identity* but kept for *span*: the
    /// decorator or `export` keyword belongs to the declaration, so a symbol
    /// slice must include it and an outline must still read `export function`.
    #[test]
    fn wrappers_are_kept_in_the_span_and_unwrapped_for_identity() {
        let source = "@decorator\ndef decorated():\n    return 2\n";
        let declarations = extract_declarations("py", source);
        let target = declarations
            .iter()
            .find(|decl| decl.name == "decorated")
            .expect("decorated fn found");
        assert_eq!(target.kind, "fn");
        assert_eq!(target.start_line, 1, "the decorator belongs to the span");
        assert_eq!(
            &source[target.byte_range.0..target.byte_range.1],
            "@decorator\ndef decorated():\n    return 2"
        );

        let ts = "export function boot() {}\n";
        let ts_target = extract_declarations("ts", ts)
            .into_iter()
            .find(|decl| decl.name == "boot")
            .expect("exported fn found");
        assert_eq!(ts_target.signature, "export function boot()");
    }
    #[test]
    fn unsupported_language_and_empty_source_yield_nothing() {
        assert!(extract_declarations("txt", "hello").is_empty());
        assert!(extract_declarations("rs", "").is_empty());
    }
}
