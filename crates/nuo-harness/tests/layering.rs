//! Layering enforcement for the `nuo-harness` subsystem groups.
//!
//! `[INV-HARNESS-03]` / `[INV-HARNESS-04]` (ADR-0100, ADR-0101): the crate is
//! organized into subsystem groups, and three of them are strict layers that
//! must never reference a group above them.
//!
//! The check is an **AST** analysis (`syn`), not a text grep: it resolves real
//! `crate::<group>` paths from `use` trees, types, and expressions, so it is
//! immune to comment mentions, string literals, and `use crate::{group::…}`
//! brace-grouping that a substring scan would miss. It also scans **macro token
//! streams** (opaque to path visitation — e.g. `vec![crate::tools::X]`), and
//! skips every `#[cfg(test)]` item (test code may construct anything and is not
//! a production edge) plus `tests.rs` submodules (whose `#[cfg(test)]` gate
//! lives on the `mod` declaration in the parent file).

// Tests panic on assertion failure by design; the workspace's unwrap/expect
// warnings are meant for production code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::TokenTree;
use syn::visit::{self, Visit};
use syn::{Attribute, Item, ItemUse, Macro, Path as SynPath, UseTree};

/// True when any attribute is `#[cfg(test)]`.
fn has_cfg_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg") && {
            let mut found = false;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("test") {
                    found = true;
                }
                Ok(())
            });
            found
        }
    })
}

/// The attributes of any item kind (`syn::Item` has no common accessor).
fn item_attrs(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(i) => &i.attrs,
        Item::Enum(i) => &i.attrs,
        Item::ExternCrate(i) => &i.attrs,
        Item::Fn(i) => &i.attrs,
        Item::ForeignMod(i) => &i.attrs,
        Item::Impl(i) => &i.attrs,
        Item::Macro(i) => &i.attrs,
        Item::Mod(i) => &i.attrs,
        Item::Static(i) => &i.attrs,
        Item::Struct(i) => &i.attrs,
        Item::Trait(i) => &i.attrs,
        Item::TraitAlias(i) => &i.attrs,
        Item::Type(i) => &i.attrs,
        Item::Union(i) => &i.attrs,
        Item::Use(i) => &i.attrs,
        _ => &[],
    }
}

/// Records the first path segment after `crate::` for every crate-rooted path,
/// `use` tree, and macro token stream it visits, skipping `#[cfg(test)]` items.
#[derive(Default)]
struct GroupCollector {
    groups: BTreeSet<String>,
}

impl GroupCollector {
    /// Walk a `use` tree positioned immediately after `crate::` (`after_crate =
    /// true`) and record each disjunct's first segment.
    fn note_after_crate(&mut self, tree: &UseTree, after_crate: bool) {
        match tree {
            UseTree::Path(path) => {
                if after_crate {
                    self.groups.insert(path.ident.to_string());
                }
                self.note_after_crate(&path.tree, false);
            }
            UseTree::Group(group) => {
                for item in &group.items {
                    self.note_after_crate(item, after_crate);
                }
            }
            UseTree::Name(name) => {
                if after_crate {
                    self.groups.insert(name.ident.to_string());
                }
            }
            UseTree::Rename(rename) => {
                if after_crate {
                    self.groups.insert(rename.ident.to_string());
                }
            }
            UseTree::Glob(_) => {}
        }
    }
}

impl<'ast> Visit<'ast> for GroupCollector {
    fn visit_item(&mut self, item: &'ast Item) {
        // Skips the whole subtree of any `#[cfg(test)]` item, whatever its kind.
        if has_cfg_test(item_attrs(item)) {
            return;
        }
        visit::visit_item(self, item);
    }

    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        if let UseTree::Path(root) = &item.tree {
            if root.ident == "crate" {
                self.note_after_crate(&root.tree, true);
                return;
            }
        }
        visit::visit_item_use(self, item);
    }

    fn visit_path(&mut self, path: &'ast SynPath) {
        if path.leading_colon.is_none() {
            let mut segments = path.segments.iter();
            if let Some(first) = segments.next() {
                if first.ident == "crate" {
                    if let Some(second) = segments.next() {
                        self.groups.insert(second.ident.to_string());
                    }
                }
            }
        }
        visit::visit_path(self, path);
    }

    fn visit_macro(&mut self, mac: &'ast Macro) {
        // A macro body is an opaque token tree to path visitation, so
        // `vec![crate::tools::X]` would otherwise be invisible. Flatten nested
        // groups (delimiters carry no `crate::` adjacency) and scan the stream.
        let mut tokens = Vec::new();
        flatten_tokens(mac.tokens.clone(), &mut tokens);
        let mut i = 0;
        while i + 3 < tokens.len() {
            let is_crate = matches!(&tokens[i], TokenTree::Ident(id) if id == "crate");
            let colon = matches!(&tokens[i + 1], TokenTree::Punct(p) if p.as_char() == ':');
            let colon2 = matches!(&tokens[i + 2], TokenTree::Punct(p) if p.as_char() == ':');
            if is_crate && colon && colon2 {
                if let TokenTree::Ident(second) = &tokens[i + 3] {
                    self.groups.insert(second.to_string());
                }
            }
            i += 1;
        }
        visit::visit_macro(self, mac);
    }
}

/// Flatten a token stream, splicing in the contents of every group.
fn flatten_tokens(stream: proc_macro2::TokenStream, out: &mut Vec<TokenTree>) {
    for token in stream {
        match token {
            TokenTree::Group(group) => flatten_tokens(group.stream(), out),
            other => out.push(other),
        }
    }
}

/// The set of top-level groups referenced by `src` outside `#[cfg(test)]`.
fn referenced_groups(src: &str) -> BTreeSet<String> {
    let Ok(file) = syn::parse_file(src) else {
        return BTreeSet::new();
    };
    let mut collector = GroupCollector::default();
    collector.visit_file(&file);
    collector.groups
}

/// Every non-`tests.rs` `.rs` file under `dir`, recursively.
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs")
            && path.file_name().is_some_and(|name| name != "tests.rs")
        {
            out.push(path);
        }
    }
}

/// Files under `src/<group>/` that reference any of `forbidden`.
fn violations(src: &Path, group: &str, forbidden: &[&str]) -> Vec<String> {
    let dir = src.join(group);
    let mut files = Vec::new();
    sources(&dir, &mut files);
    assert!(!files.is_empty(), "expected sources under {dir:?}");

    let mut out = Vec::new();
    for file in &files {
        let text = fs::read_to_string(file).expect("read source");
        let referenced = referenced_groups(&text);
        for name in forbidden {
            if referenced.contains(*name) {
                out.push(format!("{} references `crate::{name}`", file.display()));
            }
        }
    }
    out
}

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

// ---------------------------------------------------------------------------
// Boundary rules
// ---------------------------------------------------------------------------

#[test]
fn governance_is_a_strict_inner_layer() {
    let found = violations(
        &src_root(),
        "governance",
        &["cognition", "orchestration", "dispatch", "tools"],
    );
    assert!(
        found.is_empty(),
        "governance is the restraint layer and must not depend on an upper group [INV-HARNESS-03]:\n{}",
        found.join("\n")
    );
}

#[test]
fn runtime_is_the_innermost_layer() {
    let found = violations(
        &src_root(),
        "runtime",
        &[
            "cognition",
            "orchestration",
            "dispatch",
            "governance",
            "tools",
        ],
    );
    assert!(
        found.is_empty(),
        "runtime is the substrate and must not depend on an upper group [INV-HARNESS-04]:\n{}",
        found.join("\n")
    );
}

#[test]
fn cognition_does_not_depend_on_orchestration_or_dispatch() {
    let found = violations(&src_root(), "cognition", &["orchestration", "dispatch"]);
    assert!(
        found.is_empty(),
        "cognition is wrapped by orchestration/dispatch and must not depend on them [INV-HARNESS-04]:\n{}",
        found.join("\n")
    );
}

#[test]
fn single_cognitive_runtime_crate() {
    // `[INV-RUNTIME-01]` (ADR-0100): `nuo-harness` is the sole cognitive-runtime
    // crate. The retired `nuo-agent` twin must never be re-created.
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    assert!(
        !workspace.join("crates").join("nuo-agent").exists(),
        "the retired `nuo-agent` cognitive-runtime crate must not be re-created [INV-RUNTIME-01]"
    );
}

// ---------------------------------------------------------------------------
// Negative controls — prove the detector actually detects (and only what it
// should). A boundary test that cannot fail is not a mechanism.
// ---------------------------------------------------------------------------

#[test]
fn detector_resolves_use_trees_types_and_brace_groups() {
    let groups = referenced_groups(
        r#"
        use crate::cognition::agent::Agent;
        use crate::{orchestration::Round, dispatch::Pipeline};
        fn prod() -> crate::runtime::host::KernelHost { todo!() }
        type Alias = crate::governance::Guard;
        const P: &str = "crate::tools::not_a_real_ref";
    "#,
    );
    for expected in [
        "cognition",
        "orchestration",
        "dispatch",
        "runtime",
        "governance",
    ] {
        assert!(groups.contains(expected), "missing {expected}: {groups:?}");
    }
    assert!(
        !groups.contains("tools"),
        "a crate:: path inside a string literal must not count: {groups:?}"
    );
}

#[test]
fn detector_scans_macro_token_streams() {
    let groups = referenced_groups(
        r#"
        fn prod() {
            let _ = vec![crate::tools::AskUserTool, crate::cognition::agent::Agent];
            assert!(matches!(x, crate::governance::Guard));
        }
    "#,
    );
    for expected in ["tools", "cognition", "governance"] {
        assert!(
            groups.contains(expected),
            "macro body missed {expected}: {groups:?}"
        );
    }
}

#[test]
fn detector_ignores_comments_and_all_cfg_test_items() {
    let groups = referenced_groups(
        r#"
        // mention of crate::cognition::agent in a comment
        /// doc link `crate::orchestration::pipeline`
        #[cfg(test)]
        mod tests { use crate::governance::Guard; }
        #[cfg(test)]
        fn helper() { let _ = crate::tools::AskUserTool; }
        #[cfg(test)]
        impl Foo { fn t(&self) { let _ = crate::dispatch::Pipeline; } }
        #[cfg(test)]
        use crate::runtime::time::unix_epoch_ms;
        fn keep() {}
    "#,
    );
    assert!(
        groups.is_empty(),
        "comments and every `#[cfg(test)]` item must be ignored: {groups:?}"
    );
}
