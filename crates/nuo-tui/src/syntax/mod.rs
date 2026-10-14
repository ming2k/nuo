//! Syntax highlighting engine for diffs and code blocks.
//!
//! Provides fast, lightweight, and zero-allocation lexical tokenization
//! for developer languages (Rust, TypeScript/JavaScript, Python, Go, JSON,
//! TOML, Shell, HTML/CSS, etc.). Designed specifically for Diff viewing where
//! code snippets are often incomplete AST fragments.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    TypeScript,
    JavaScript,
    Python,
    Go,
    Json,
    Toml,
    Yaml,
    Shell,
    Html,
    Css,
    Markdown,
    Plain,
}

impl Language {
    /// Detect programming language from file path or extension.
    pub fn from_path<P: AsRef<Path>>(path: P) -> Self {
        let p = path.as_ref();
        let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
        let file_name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");

        match file_name.to_ascii_lowercase().as_str() {
            "dockerfile" => return Language::Shell,
            "makefile" => return Language::Shell,
            "cargo.lock" => return Language::Toml,
            _ => {}
        }

        match ext.to_ascii_lowercase().as_str() {
            "rs" => Language::Rust,
            "ts" | "tsx" | "mts" | "cts" => Language::TypeScript,
            "js" | "jsx" | "mjs" | "cjs" => Language::JavaScript,
            "py" | "pyi" => Language::Python,
            "go" => Language::Go,
            "json" | "jsonc" | "json5" => Language::Json,
            "toml" => Language::Toml,
            "yaml" | "yml" => Language::Yaml,
            "sh" | "bash" | "zsh" | "fish" => Language::Shell,
            "html" | "htm" => Language::Html,
            "css" | "scss" | "less" => Language::Css,
            "md" | "markdown" => Language::Markdown,
            _ => Language::Plain,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxKind {
    Keyword,
    Type,
    Function,
    String,
    Number,
    Comment,
    Constant,
    Operator,
    Punctuation,
    Plain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxSpan {
    pub start_byte: usize,
    pub end_byte: usize,
    pub kind: SyntaxKind,
}

/// Tokenize a single line of code into non-overlapping syntax spans covering 0..line.len().
pub fn tokenize_line(line: &str, lang: Language) -> Vec<SyntaxSpan> {
    if line.is_empty() || lang == Language::Plain {
        return vec![SyntaxSpan {
            start_byte: 0,
            end_byte: line.len(),
            kind: SyntaxKind::Plain,
        }];
    }

    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut spans = Vec::new();
    let mut i = 0;

    while i < len {
        let b = bytes[i];

        // 1. Comments
        if matches!(
            lang,
            Language::Rust
                | Language::TypeScript
                | Language::JavaScript
                | Language::Go
                | Language::Css
        ) && i + 1 < len
            && b == b'/'
            && bytes[i + 1] == b'/'
        {
            spans.push(SyntaxSpan {
                start_byte: i,
                end_byte: len,
                kind: SyntaxKind::Comment,
            });
            break;
        }
        if matches!(
            lang,
            Language::Python | Language::Shell | Language::Toml | Language::Yaml
        ) && b == b'#'
        {
            spans.push(SyntaxSpan {
                start_byte: i,
                end_byte: len,
                kind: SyntaxKind::Comment,
            });
            break;
        }

        // 2. Strings
        if b == b'"'
            || b == b'\''
            || (b == b'`'
                && matches!(
                    lang,
                    Language::JavaScript
                        | Language::TypeScript
                        | Language::Go
                        | Language::Markdown
                        | Language::Shell
                ))
        {
            let quote = b;
            let start = i;
            i += 1;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2; // skip escaped char
                } else if bytes[i] == quote {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            spans.push(SyntaxSpan {
                start_byte: start,
                end_byte: i,
                kind: SyntaxKind::String,
            });
            continue;
        }

        // 3. Numbers
        if b.is_ascii_digit() && (i == 0 || !is_ident_char(bytes[i - 1])) {
            let start = i;
            if b == b'0'
                && i + 1 < len
                && (bytes[i + 1] == b'x'
                    || bytes[i + 1] == b'X'
                    || bytes[i + 1] == b'b'
                    || bytes[i + 1] == b'o')
            {
                i += 2;
                while i < len && (bytes[i].is_ascii_hexdigit() || bytes[i] == b'_') {
                    i += 1;
                }
            } else {
                while i < len && (bytes[i].is_ascii_digit() || bytes[i] == b'.' || bytes[i] == b'_')
                {
                    i += 1;
                }
            }
            spans.push(SyntaxSpan {
                start_byte: start,
                end_byte: i,
                kind: SyntaxKind::Number,
            });
            continue;
        }

        // 4. Identifiers & Keywords
        if is_ident_start(b) {
            let start = i;
            while i < len && is_ident_char(bytes[i]) {
                i += 1;
            }
            let word = &line[start..i];

            // Check if immediately followed by `(` -> Function
            let mut is_fn = false;
            let mut peek = i;
            while peek < len && bytes[peek].is_ascii_whitespace() {
                peek += 1;
            }
            if peek < len && bytes[peek] == b'(' {
                is_fn = true;
            }

            let kind = classify_ident(word, lang, is_fn);
            spans.push(SyntaxSpan {
                start_byte: start,
                end_byte: i,
                kind,
            });
            continue;
        }

        // 5. Operators & Punctuation
        let start = i;
        if is_operator(b) {
            while i < len && is_operator(bytes[i]) {
                i += 1;
            }
            spans.push(SyntaxSpan {
                start_byte: start,
                end_byte: i,
                kind: SyntaxKind::Operator,
            });
            continue;
        }

        // Single punctuation, whitespace, or multi-byte UTF-8 grapheme
        let ch = line[i..].chars().next().unwrap_or(' ');
        let char_len = ch.len_utf8();
        let end = i + char_len;
        i = end;
        spans.push(SyntaxSpan {
            start_byte: start,
            end_byte: end,
            kind: if ch.is_ascii_punctuation() {
                SyntaxKind::Punctuation
            } else {
                SyntaxKind::Plain
            },
        });
    }

    spans
}

#[inline]
fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'$'
}

#[inline]
fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

#[inline]
fn is_operator(b: u8) -> bool {
    matches!(
        b,
        b'+' | b'-'
            | b'*'
            | b'/'
            | b'%'
            | b'='
            | b'<'
            | b'>'
            | b'!'
            | b'&'
            | b'|'
            | b'^'
            | b'~'
            | b'?'
    )
}

fn classify_ident(word: &str, lang: Language, is_fn: bool) -> SyntaxKind {
    // 1. Check booleans / constants
    if matches!(
        word,
        "true" | "false" | "null" | "None" | "nil" | "undefined" | "NaN" | "Some" | "Ok" | "Err"
    ) {
        return SyntaxKind::Constant;
    }

    // 2. Language-specific keywords
    let is_kw = match lang {
        Language::Rust => matches!(
            word,
            "as" | "break"
                | "const"
                | "continue"
                | "crate"
                | "else"
                | "enum"
                | "extern"
                | "false"
                | "fn"
                | "for"
                | "if"
                | "impl"
                | "in"
                | "let"
                | "loop"
                | "match"
                | "mod"
                | "move"
                | "mut"
                | "pub"
                | "ref"
                | "return"
                | "self"
                | "Self"
                | "static"
                | "struct"
                | "super"
                | "trait"
                | "true"
                | "type"
                | "unsafe"
                | "use"
                | "where"
                | "while"
                | "async"
                | "await"
                | "dyn"
        ),
        Language::TypeScript | Language::JavaScript => matches!(
            word,
            "break"
                | "case"
                | "catch"
                | "class"
                | "const"
                | "continue"
                | "debugger"
                | "default"
                | "delete"
                | "do"
                | "else"
                | "export"
                | "extends"
                | "finally"
                | "for"
                | "function"
                | "if"
                | "import"
                | "in"
                | "instanceof"
                | "new"
                | "return"
                | "super"
                | "switch"
                | "this"
                | "throw"
                | "try"
                | "typeof"
                | "var"
                | "void"
                | "while"
                | "with"
                | "yield"
                | "async"
                | "await"
                | "interface"
                | "type"
                | "from"
                | "as"
        ),
        Language::Python => matches!(
            word,
            "and"
                | "as"
                | "assert"
                | "async"
                | "await"
                | "break"
                | "class"
                | "continue"
                | "def"
                | "del"
                | "elif"
                | "else"
                | "except"
                | "finally"
                | "for"
                | "from"
                | "global"
                | "if"
                | "import"
                | "in"
                | "is"
                | "lambda"
                | "nonlocal"
                | "not"
                | "or"
                | "pass"
                | "raise"
                | "return"
                | "try"
                | "while"
                | "with"
                | "yield"
        ),
        Language::Go => matches!(
            word,
            "break"
                | "default"
                | "func"
                | "interface"
                | "select"
                | "case"
                | "defer"
                | "go"
                | "map"
                | "struct"
                | "chan"
                | "else"
                | "goto"
                | "package"
                | "switch"
                | "const"
                | "fallthrough"
                | "if"
                | "range"
                | "type"
                | "continue"
                | "for"
                | "import"
                | "return"
                | "var"
        ),
        Language::Shell => matches!(
            word,
            "if" | "then"
                | "else"
                | "elif"
                | "fi"
                | "case"
                | "esac"
                | "for"
                | "while"
                | "until"
                | "do"
                | "done"
                | "in"
                | "function"
                | "return"
                | "exit"
                | "export"
                | "local"
        ),
        Language::Toml | Language::Yaml | Language::Json => false,
        _ => false,
    };

    if is_kw {
        return SyntaxKind::Keyword;
    }

    // 3. Types (capitalized identifier or common primitive types)
    if matches!(
        word,
        "i8" | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "f32"
            | "f64"
            | "str"
            | "bool"
            | "char"
            | "String"
            | "Vec"
            | "Option"
            | "Result"
            | "int"
            | "float"
            | "boolean"
            | "number"
            | "string"
            | "any"
            | "void"
    ) {
        return SyntaxKind::Type;
    }

    if word.starts_with(|c: char| c.is_ascii_uppercase())
        && !word.chars().all(|c| c.is_ascii_uppercase() || c == '_')
    {
        return SyntaxKind::Type;
    }

    // 4. Function call
    if is_fn {
        return SyntaxKind::Function;
    }

    SyntaxKind::Plain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_detection() {
        assert_eq!(Language::from_path("src/main.rs"), Language::Rust);
        assert_eq!(Language::from_path("index.ts"), Language::TypeScript);
        assert_eq!(Language::from_path("app.py"), Language::Python);
        assert_eq!(Language::from_path("main.go"), Language::Go);
        assert_eq!(Language::from_path("config.toml"), Language::Toml);
        assert_eq!(Language::from_path("script.sh"), Language::Shell);
        assert_eq!(Language::from_path("unknown.xyz"), Language::Plain);
    }

    #[test]
    fn test_tokenize_rust_line() {
        let line = "pub fn calculate(x: i32) -> String {";
        let tokens = tokenize_line(line, Language::Rust);

        let kw_pub = tokens
            .iter()
            .find(|t| &line[t.start_byte..t.end_byte] == "pub")
            .unwrap();
        assert_eq!(kw_pub.kind, SyntaxKind::Keyword);

        let kw_fn = tokens
            .iter()
            .find(|t| &line[t.start_byte..t.end_byte] == "fn")
            .unwrap();
        assert_eq!(kw_fn.kind, SyntaxKind::Keyword);

        let fn_calc = tokens
            .iter()
            .find(|t| &line[t.start_byte..t.end_byte] == "calculate")
            .unwrap();
        assert_eq!(fn_calc.kind, SyntaxKind::Function);

        let type_i32 = tokens
            .iter()
            .find(|t| &line[t.start_byte..t.end_byte] == "i32")
            .unwrap();
        assert_eq!(type_i32.kind, SyntaxKind::Type);

        let type_str = tokens
            .iter()
            .find(|t| &line[t.start_byte..t.end_byte] == "String")
            .unwrap();
        assert_eq!(type_str.kind, SyntaxKind::Type);
    }

    #[test]
    fn test_tokenize_strings_and_comments() {
        let line = "let msg = \"hello world\"; // greeting";
        let tokens = tokenize_line(line, Language::Rust);

        let str_tok = tokens
            .iter()
            .find(|t| t.kind == SyntaxKind::String)
            .unwrap();
        assert_eq!(
            &line[str_tok.start_byte..str_tok.end_byte],
            "\"hello world\""
        );

        let comment_tok = tokens
            .iter()
            .find(|t| t.kind == SyntaxKind::Comment)
            .unwrap();
        assert_eq!(
            &line[comment_tok.start_byte..comment_tok.end_byte],
            "// greeting"
        );
    }
}
