//! The structural-query grammar: a closed, human-writable way to ask "where is
//! this declaration?" (ADR-0237).
//!
//! A tree-sitter query language (S-expression patterns) is powerful and
//! completely unsuitable as a model-facing surface: a malformed pattern is a
//! runtime failure the caller cannot diagnose, and the acceptable node-kind
//! vocabulary differs per grammar. This module defines the opposite trade — a
//! tiny grammar over the closed kind vocabulary in
//! [`crate::syntax::declarations::DECLARATION_KINDS`], where a bad clause is rejected with the
//! legal values named.
//!
//! ```text
//! pattern := clause ( ( ',' | whitespace ) clause )*
//! clause  := kind [ ':' name_glob ]
//! kind    := fn | method | struct | enum | trait | impl | class
//!          | interface | type | const | static | mod | macro
//! ```
//!
//! Clauses are ORed: `fn:handle_* , struct` matches handle-prefixed functions
//! and every struct. `name_glob` supports `*` (any run) and `?` (one
//! character) — no character classes, no alternation, no path semantics.
//!
//! One asymmetry is deliberate: the `fn` clause also matches `method`, because
//! "where is this function?" is the question a caller actually has, and whether
//! the function happens to live inside an `impl`/`class` is an implementation
//! detail of the caller's mental model, not of its intent. `method` stays
//! exact.

use super::declarations::{DECLARATION_KINDS, Declaration};

/// One OR arm of a parsed pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clause {
    /// Canonical kind, always a member of [`DECLARATION_KINDS`].
    pub kind: String,
    /// Optional name glob; `None` matches every name of that kind.
    pub name_glob: Option<String>,
}

/// A parsed structural query: one or more ORed clauses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxPattern {
    pub clauses: Vec<Clause>,
}

impl SyntaxPattern {
    /// Does `declaration` satisfy any clause?
    pub fn matches(&self, declaration: &Declaration) -> bool {
        self.clauses.iter().any(|clause| {
            kind_matches(&clause.kind, declaration)
                && match &clause.name_glob {
                    None => true,
                    Some(glob) => matches_name(glob, &declaration.name),
                }
        })
    }
}

/// Kind matching for a clause.
///
/// `fn` is the general function-like kind and **also** matches members, so a
/// caller asking "where is `handle_*` defined?" does not have to know whether
/// the function happens to sit inside an `impl` or `class` — the single most
/// likely way a structural search would silently miss its target. `method`
/// stays exact, for when the member position *is* the point.
fn kind_matches(clause_kind: &str, declaration: &Declaration) -> bool {
    clause_kind == declaration.kind || (clause_kind == "fn" && declaration.kind == "method")
}

/// Parse a model-supplied pattern. Every failure names the legal vocabulary
/// instead of returning an empty result, so a miss is diagnosable.
pub fn parse_syntax_pattern(input: &str) -> Result<SyntaxPattern, String> {
    let mut clauses = Vec::new();
    for raw in input.split([',', ' ', '\t', '\n']) {
        let clause = raw.trim();
        if clause.is_empty() {
            continue;
        }
        let (kind, name_glob) = match clause.split_once(':') {
            Some((kind, glob)) => (kind.trim(), Some(glob.trim())),
            None => (clause, None),
        };
        if kind.is_empty() {
            return Err(format!(
                "Pattern clause '{clause}' is missing a kind before ':'. \
                 Use '<kind>[:<name-glob>]', e.g. 'fn:handle_*'."
            ));
        }
        if !DECLARATION_KINDS.contains(&kind) {
            return Err(format!(
                "Unknown declaration kind '{kind}' in pattern clause '{clause}'. \
                 Valid kinds: {}. Example: 'fn:handle_* , class'.",
                DECLARATION_KINDS.join(", ")
            ));
        }
        if let Some(glob) = name_glob {
            if glob.is_empty() {
                return Err(format!(
                    "Pattern clause '{clause}' has an empty name glob after ':'. \
                     Either drop the ':' (match every {kind}) or name a glob such as \
                     '{kind}:handle_*'."
                ));
            }
            if glob.contains("**") || glob.contains('[') || glob.contains('{') {
                return Err(format!(
                    "Name glob '{glob}' uses unsupported syntax. Only '*' (any run) and \
                     '?' (one character) are supported; character classes, brace \
                     alternation, and '**' are not."
                ));
            }
        }
        clauses.push(Clause {
            kind: kind.to_string(),
            name_glob: name_glob.map(str::to_string),
        });
    }
    if clauses.is_empty() {
        return Err(format!(
            "Empty structural pattern. Provide at least one kind clause from: {}.",
            DECLARATION_KINDS.join(", ")
        ));
    }
    Ok(SyntaxPattern { clauses })
}

/// Glob match supporting `*` (any run, including empty) and `?` (exactly one
/// character). Iterative with backtracking, so a pathological pattern cannot
/// blow the stack.
pub fn matches_name(glob: &str, name: &str) -> bool {
    let glob: Vec<char> = glob.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let (mut g, mut n) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;

    while n < name.len() {
        match glob.get(g) {
            Some('*') => {
                star = Some((g, n));
                g += 1;
            }
            Some('?') => {
                g += 1;
                n += 1;
            }
            Some(expected) if *expected == name[n] => {
                g += 1;
                n += 1;
            }
            _ => match star {
                Some((star_g, star_n)) => {
                    g = star_g + 1;
                    n = star_n + 1;
                    star = Some((star_g, star_n + 1));
                }
                None => return false,
            },
        }
    }
    while glob.get(g) == Some(&'*') {
        g += 1;
    }
    g == glob.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kind_only_and_kind_with_glob_clauses() {
        let pattern = parse_syntax_pattern("fn, struct:User*").unwrap();
        assert_eq!(
            pattern.clauses,
            vec![
                Clause {
                    kind: "fn".into(),
                    name_glob: None
                },
                Clause {
                    kind: "struct".into(),
                    name_glob: Some("User*".into())
                },
            ]
        );
    }

    #[test]
    fn whitespace_separates_clauses_just_like_commas() {
        let pattern = parse_syntax_pattern("  fn:handle_*   class  ").unwrap();
        assert_eq!(pattern.clauses.len(), 2);
    }

    #[test]
    fn rejects_unknown_kinds_and_names_the_vocabulary() {
        let error = parse_syntax_pattern("functon:x").unwrap_err();
        assert!(
            error.contains("Unknown declaration kind 'functon'"),
            "{error}"
        );
        assert!(error.contains("fn"), "must list valid kinds: {error}");
    }

    #[test]
    fn rejects_unsupported_glob_syntax_instead_of_running_regex() {
        for pattern in ["fn:a**", "fn:a[bc]", "fn:{a,b}"] {
            let error = parse_syntax_pattern(pattern).unwrap_err();
            assert!(error.contains("unsupported syntax"), "{pattern}: {error}");
        }
    }

    #[test]
    fn rejects_empty_pattern_and_empty_glob() {
        assert!(parse_syntax_pattern("   ").unwrap_err().contains("Empty"));
        assert!(
            parse_syntax_pattern("fn:")
                .unwrap_err()
                .contains("empty name glob")
        );
    }

    #[test]
    fn glob_matching_covers_runs_questions_and_anchoring() {
        assert!(matches_name("handle_*", "handle_request"));
        assert!(matches_name("*_request", "handle_request"));
        assert!(matches_name("*", ""));
        assert!(matches_name("a?c", "abc"));
        assert!(!matches_name("a?c", "ac"));
        assert!(!matches_name("handle_*", "pre_handle_x"));
        assert!(!matches_name("abc", "abcd"));
        assert!(!matches_name("", "a"));
        assert!(matches_name("", ""));
    }

    #[test]
    fn matches_declarations_by_kind_and_name() {
        let source = "pub fn handle_a() {}\npub fn other() {}\npub struct Handler;\n";
        let declarations = super::super::declarations::extract_declarations("rs", source);
        let pattern = parse_syntax_pattern("fn:handle_*").unwrap();
        let matched: Vec<&str> = declarations
            .iter()
            .filter(|decl| pattern.matches(decl))
            .map(|decl| decl.name.as_str())
            .collect();
        assert_eq!(matched, vec!["handle_a"]);
    }

    /// `fn` matches members too, so a caller need not know whether a function
    /// sits inside an `impl`/`class`; `method` remains exact.
    #[test]
    fn fn_clause_also_matches_members_but_method_stays_exact() {
        let source = "impl S {\n    fn member() {}\n}\nfn top() {}\n";
        let declarations = super::super::declarations::extract_declarations("rs", source);

        let any_fn = parse_syntax_pattern("fn").unwrap();
        let fn_names: Vec<&str> = declarations
            .iter()
            .filter(|decl| any_fn.matches(decl))
            .map(|decl| decl.name.as_str())
            .collect();
        assert!(fn_names.contains(&"member"), "{fn_names:?}");
        assert!(fn_names.contains(&"top"), "{fn_names:?}");

        let only_methods = parse_syntax_pattern("method").unwrap();
        let method_names: Vec<&str> = declarations
            .iter()
            .filter(|decl| only_methods.matches(decl))
            .map(|decl| decl.name.as_str())
            .collect();
        assert_eq!(method_names, vec!["member"]);
    }
}
