use super::*;
use tempfile::tempdir;

const SERVICE: &str = "\
pub struct Service;

impl Service {
    pub fn handle_request(&self) -> u32 {
        1
    }

    fn helper(&self) {}
}

pub fn top_level() {}
";

fn tool_for(root: &Path) -> CodeQueryTool {
    CodeQueryTool::new(Some(root.to_path_buf()))
}

/// A structural result must identify the source version it
/// describes, and that identity must track the bytes on disk — it is the
/// value a later `expected_version` write is checked against.
#[tokio::test]
async fn outline_reports_a_version_that_tracks_content() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("service.rs");
    std::fs::write(&file, SERVICE).unwrap();
    let tool = tool_for(dir.path());

    let first = tool
        .call(r#"{"mode":"outline","path":"service.rs"}"#)
        .await
        .unwrap();
    assert!(first.contains("version "), "{first}");
    assert!(first.contains("pub struct Service"), "{first}");
    // Members are reachable, indented under their container.
    assert!(
        first.contains("Service::handle_request") || first.contains("pub fn handle_request"),
        "{first}"
    );

    let repeat = tool
        .call(r#"{"mode":"outline","path":"service.rs"}"#)
        .await
        .unwrap();
    assert_eq!(first.lines().next(), repeat.lines().next());

    // Same-size replacement still changes the version.
    std::fs::write(&file, SERVICE.replace("top_level", "top_levxl")).unwrap();
    let second = tool
        .call(r#"{"mode":"outline","path":"service.rs"}"#)
        .await
        .unwrap();
    assert_ne!(first.lines().next(), second.lines().next());
}

/// The whole point of `symbol` mode: the declaration's source, so a caller
/// does not have to read (and pay for) the entire file.
#[tokio::test]
async fn symbol_returns_the_declaration_source_and_its_span() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("service.rs"), SERVICE).unwrap();
    let tool = tool_for(dir.path());

    let found = tool
        .call(r#"{"mode":"symbol","path":"service.rs","symbol":"handle_request"}"#)
        .await
        .unwrap();
    assert!(
        found.contains("pub fn handle_request(&self) -> u32"),
        "{found}"
    );
    assert!(
        found.contains("        1"),
        "body must be included: {found}"
    );
    assert!(found.contains("service.rs:4-6"), "{found}");
    assert!(
        !found.contains("fn helper"),
        "siblings must not be inlined: {found}"
    );
}

/// `symbol` accepts a container-qualified name so same-named members in
/// different containers can be told apart.
#[tokio::test]
async fn symbol_resolves_a_qualified_name() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("service.rs"), SERVICE).unwrap();
    let tool = tool_for(dir.path());

    let qualified = tool
        .call(r#"{"mode":"symbol","path":"service.rs","symbol":"Service::helper"}"#)
        .await
        .unwrap();
    assert!(qualified.contains("fn helper(&self)"), "{qualified}");

    let missing = tool
        .call(r#"{"mode":"symbol","path":"service.rs","symbol":"Nope::helper"}"#)
        .await
        .unwrap();
    assert!(missing.contains("No declaration named"), "{missing}");
}

/// `find` locates declarations by kind and name glob across a scope.
#[tokio::test]
async fn find_matches_kind_and_name_glob_across_a_scope() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), SERVICE).unwrap();
    std::fs::write(
        dir.path().join("src/b.rs"),
        "pub fn handle_other() {}\npub struct Handler;\n",
    )
    .unwrap();

    let tool = tool_for(dir.path());
    let globbed = tool
        .call(r#"{"mode":"find","pattern":"fn:handle_*","path":"src"}"#)
        .await
        .unwrap();
    assert!(globbed.contains("handle_request"), "{globbed}");
    assert!(globbed.contains("handle_other"), "{globbed}");
    assert!(
        globbed.contains("src/a.rs") && globbed.contains("src/b.rs"),
        "{globbed}"
    );
    assert!(!globbed.contains("top_level"), "{globbed}");

    let kinds = tool
        .call(r#"{"mode":"find","pattern":"struct"}"#)
        .await
        .unwrap();
    assert!(kinds.contains("struct Service"), "{kinds}");
    assert!(kinds.contains("struct Handler"), "{kinds}");
    assert!(!kinds.contains("handle_other"), "{kinds}");
}

/// A bad kind is rejected with the legal vocabulary named — never a silent
/// empty result.
#[tokio::test]
async fn find_rejects_unknown_kinds_with_the_vocabulary() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), SERVICE).unwrap();
    let tool = tool_for(dir.path());

    let error = tool
        .call(r#"{"mode":"find","pattern":"fnction:x"}"#)
        .await
        .unwrap_err();
    assert!(
        error.contains("Unknown declaration kind 'fnction'"),
        "{error}"
    );
    assert!(error.contains("method"), "{error}");
}

/// Each mode names the argument it requires, so a missing field is a
/// next-step instruction rather than a bare parse failure.
#[tokio::test]
async fn modes_name_their_required_argument() {
    let dir = tempdir().unwrap();
    let tool = tool_for(dir.path());
    assert!(
        tool.call(r#"{"mode":"outline"}"#)
            .await
            .unwrap_err()
            .contains("requires 'path'")
    );
    assert!(
        tool.call(r#"{"mode":"symbol"}"#)
            .await
            .unwrap_err()
            .contains("requires 'symbol'")
    );
    assert!(
        tool.call(r#"{"mode":"find"}"#)
            .await
            .unwrap_err()
            .contains("requires 'pattern'")
    );
}

/// The mode enum must be advertised as a string enum, for the same reason
/// the dispatch role enum must be (invariant 6): the model reads
/// the schema, and a value it cannot see is a value it cannot use. It is
/// generated from the same const the parser accepts against, so the
/// advertised set and the enforced set are equal by construction.
#[test]
fn schema_advertises_the_mode_enum_as_a_string_enum() {
    let tool = CodeQueryTool::new(None);
    let mode = &tool.parameters()["properties"]["mode"];
    assert_eq!(
        mode["type"], "string",
        "a string field must not be schema'd as an object"
    );
    let modes: Vec<String> = mode["enum"]
        .as_array()
        .expect("mode enum is an array")
        .iter()
        .map(|value| value.as_str().expect("modes are strings").to_string())
        .collect();
    assert_eq!(modes, vec!["outline", "symbol", "find"]);

    // Every advertised value must parse, and nothing else may.
    for advertised in &modes {
        assert!(Mode::parse(advertised).is_ok(), "{advertised} must parse");
    }
    assert!(Mode::parse("OUTLINE").is_ok(), "case should not matter");
    let rejection = Mode::parse("summarize").unwrap_err();
    assert!(
        rejection.contains("Unknown 'mode' 'summarize'"),
        "{rejection}"
    );
    assert!(rejection.contains("find"), "{rejection}");
}

/// A qualified lookup must not cross an identifier boundary: `Service::run`
/// must not match `FooService::run`, or a caller asking for one type's
/// method would silently receive another's.
#[tokio::test]
async fn symbol_qualification_does_not_cross_identifier_boundaries() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("svc.rs"),
        "impl FooService {\n    fn run(&self) {}\n}\n",
    )
    .unwrap();
    let tool = tool_for(dir.path());

    let miss = tool
        .call(r#"{"mode":"symbol","path":"svc.rs","symbol":"Service::run"}"#)
        .await
        .unwrap();
    assert!(miss.contains("No declaration named"), "{miss}");

    let hit = tool
        .call(r#"{"mode":"symbol","path":"svc.rs","symbol":"FooService::run"}"#)
        .await
        .unwrap();
    assert!(hit.contains("fn run(&self)"), "{hit}");
}

/// A declaration longer than the output budget is disclosed as truncated,
/// including when the last line is the one that does not fit — a silently
/// dropped tail would read as a complete declaration.
#[tokio::test]
async fn symbol_discloses_a_truncated_declaration() {
    let dir = tempdir().unwrap();
    let mut source = String::from("pub fn big() {\n");
    for index in 0..30 {
        source.push_str(&format!("    let line_{index} = {index};\n"));
    }
    source.push_str("}\n");
    std::fs::write(dir.path().join("big.rs"), source).unwrap();
    let tool = tool_for(dir.path());

    let out = tool
        .call(r#"{"mode":"symbol","path":"big.rs","symbol":"big","budget":120}"#)
        .await
        .unwrap();
    assert!(out.contains("Source truncated"), "{out}");
    assert!(out.contains("pub fn big() {"), "{out}");
}

/// A scope-wide query that cannot read every file must say so: an
/// incomplete scan reported as complete is the failure mode that makes
/// structural search untrustworthy.
#[tokio::test]
async fn scope_scan_discloses_files_it_could_not_analyse() {
    let dir = tempdir().unwrap();
    let unreadable = dir.path().join("locked.rs");
    std::fs::write(&unreadable, "pub fn hidden() {}\n").unwrap();
    std::fs::write(dir.path().join("readable.rs"), "pub fn visible() {}\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
    }

    let tool = tool_for(dir.path());
    let out = tool
        .call(r#"{"mode":"find","pattern":"fn"}"#)
        .await
        .unwrap();

    assert!(out.contains("visible"), "{out}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Running as root bypasses mode bits, so only assert the disclosure
        // when the file really was unreadable.
        if std::fs::read(&unreadable).is_err() {
            assert!(out.contains("could not be analysed"), "{out}");
            assert!(!out.contains("hidden"), "{out}");
        }
        let _ = std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o644));
    }
}

/// Structure output has a finite budget and discloses
/// truncation rather than silently clipping or emitting everything.
#[tokio::test]
async fn outline_discloses_truncation_at_the_entry_limit() {
    let dir = tempdir().unwrap();
    let mut source = String::new();
    for index in 0..40 {
        source.push_str(&format!("pub fn item_{index}() {{}}\n"));
    }
    std::fs::write(dir.path().join("many.rs"), source).unwrap();
    let tool = tool_for(dir.path());

    let out = tool
        .call(r#"{"mode":"outline","path":"many.rs","limit":5}"#)
        .await
        .unwrap();
    assert!(out.contains("5 of 40 symbols"), "{out}");
    assert!(out.contains("output truncated"), "{out}");
    assert!(!out.contains("item_5"), "{out}");
}

/// The *input* is bounded too, not only the rendered output.
#[tokio::test]
async fn rejects_source_above_the_parse_budget() {
    let dir = tempdir().unwrap();
    let mut source = String::new();
    while source.len() as u64 <= MAX_SOURCE_BYTES {
        source.push_str("pub struct Pad;\n");
    }
    std::fs::write(dir.path().join("huge.rs"), source).unwrap();
    let tool = tool_for(dir.path());

    let error = tool
        .call(r#"{"mode":"outline","path":"huge.rs"}"#)
        .await
        .unwrap_err();
    assert!(error.contains("input budget"), "{error}");
}

/// Unsupported formats fail loudly instead of being reported as parsed.
#[tokio::test]
async fn rejects_unsupported_language() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "hello\n").unwrap();
    let tool = tool_for(dir.path());

    let error = tool
        .call(r#"{"mode":"outline","path":"notes.txt"}"#)
        .await
        .unwrap_err();
    assert!(error.contains("Unsupported file type"), "{error}");
}

/// A directory is a scope, not a file: `outline` says so and points at the
/// mode that takes a scope.
#[tokio::test]
async fn outline_points_a_directory_at_find_mode() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    let tool = tool_for(dir.path());

    let error = tool
        .call(r#"{"mode":"outline","path":"src"}"#)
        .await
        .unwrap_err();
    assert!(error.contains("is a directory"), "{error}");
    assert!(error.contains("mode 'find'"), "{error}");
}

/// Scope-wide queries obey the walker's ignore rules, so a `find` never
/// wades into build output or vendored trees.
#[tokio::test]
async fn find_respects_project_ignore_rules() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("target")).unwrap();
    std::fs::write(dir.path().join("keep.rs"), "pub fn keep_me() {}\n").unwrap();
    std::fs::write(dir.path().join("target/gen.rs"), "pub fn generated() {}\n").unwrap();
    let tool = tool_for(dir.path());

    let out = tool
        .call(r#"{"mode":"find","pattern":"fn"}"#)
        .await
        .unwrap();
    assert!(out.contains("keep_me"), "{out}");
    assert!(!out.contains("generated"), "{out}");
}
