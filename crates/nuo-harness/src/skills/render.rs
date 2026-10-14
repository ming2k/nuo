//! Formatting skills for resolving user mentions.

use super::metadata::Skill;

/// Build a verbose listing similar to what list_skills returns.
pub fn format_skill_list(skills: &[Skill]) -> String {
    let mut lines = vec!["Available skills:".to_string()];
    for skill in skills {
        let state = if skill.enabled { "" } else { " (disabled)" };
        lines.push(format!(
            "- [{}] {}{}\n  {}",
            skill.scope,
            skill.name,
            state,
            if skill.description.as_str().trim().is_empty() {
                "No description"
            } else {
                skill.description.as_str()
            }
        ));
    }
    lines.join("\n")
}

const MAX_LISTED_SKILL_FILES: usize = 10;

/// Collect auxiliary files inside a skill's directory (excluding `SKILL.md`).
pub fn list_skill_files(root: &std::path::Path) -> Vec<String> {
    if !root.is_dir() {
        return Vec::new();
    }
    let mut files: Vec<String> = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .max_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.file_name().map(|n| n == "SKILL.md").unwrap_or(false) {
            continue;
        }
        if let Ok(rel) = path.strip_prefix(root) {
            files.push(rel.to_string_lossy().to_string());
        }
        if files.len() >= MAX_LISTED_SKILL_FILES {
            break;
        }
    }
    files
}

/// Format a skill injection into a structured XML envelope.
///
/// Wraps domain skill instructions in a semantic `<skill>` XML block with clear
/// meta-prompting guidelines, scope, root directory, and auxiliary file listings.
/// This guides the model to treat the content as authoritative standard operating
/// procedures (SOP).
///
/// The `ref` attribute carries the **canonical address** of the skill
/// (`@skill:{name}`, ADR-0288), mirroring `<file ref="…">`: the provider reads
/// an asset reference, never a bare `@`-token whose spelling depends on how the
/// user happened to write it.
pub fn format_skill_injection(skill: &Skill, content: &str) -> String {
    let files = list_skill_files(&skill.root);
    let files_desc = if files.is_empty() {
        String::new()
    } else {
        format!(
            "\nAuxiliary files (relative to root):\n{}",
            files.join("\n")
        )
    };

    format!(
        "<skill name=\"{}\" scope=\"{}\" ref=\"@skill:{}\">\n\
         <system_guidance>\n\
         The user activated domain skill \"{}\". Follow these standard operating procedures (SOP), \
         constraints, and guidelines for all subsequent related tasks.\n\
         Skill Root: {}{}\n\
         </system_guidance>\n\
         <instructions>\n\
         {}\n\
         </instructions>\n\
         </skill>",
        skill.name,
        skill.scope,
        skill.name,
        skill.name,
        skill.root.display(),
        files_desc,
        content
    )
}

/// Resolve which skills a piece of text is referring to.
///
/// Matches only explicit intent:
/// - `@skill-name`
/// - `@skill:skill-name` or `@skills:skill-name` (disambiguated namespace)
/// - `skill://skill-name` or `skill://path/to/SKILL.md`
///
/// A plain skill name as a loose token does NOT match — that was too eager
/// and pulled skill bodies into context on coincidental word overlap.
///
/// Matching is **token-boundary aware**: mentions are scanned out of the text
/// as whole identifiers by the shared grammar kernel
/// ([`nuo_wire::mention::scan_references`]), then compared for equality.
/// So `@rust-expert` does not match a skill named `rust` (the identifier runs on
/// past it), and neither does `@skill:rust-expert`. Escaped mentions
/// (`\@skill:…`) are literal text and do not match.
pub fn resolve_mentions<'a>(text: &str, skills: &'a [Skill]) -> Vec<&'a Skill> {
    use nuo_wire::mention::{Form, Namespace, scan_references};

    let references = scan_references(text);
    let mut names: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut uris: Vec<&str> = Vec::new();
    for reference in &references {
        if reference.escaped || reference.namespace != Namespace::Skill {
            continue;
        }
        match reference.form {
            Form::Uri => uris.push(reference.target),
            Form::Bare | Form::Qualified => {
                names.insert(reference.target);
            }
        }
    }
    if names.is_empty() && uris.is_empty() {
        return Vec::new();
    }

    let mut matched = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for skill in skills
        .iter()
        .filter(|s| s.enabled && s.allows_implicit_invocation())
    {
        let uri_hit = uris
            .iter()
            .any(|uri| *uri == skill.name || *uri == skill.source.to_string_lossy());
        let hit = names.contains(skill.name.as_str()) || uri_hit;
        if hit && seen.insert(skill.name.clone()) {
            matched.push(skill);
        }
    }

    matched
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample_skill(name: &str) -> Skill {
        Skill {
            name: name.to_string(),
            description: "desc".to_string(),
            short_description: None,
            scope: crate::SkillScope::Repo,
            source: PathBuf::from(format!("skills/{}/SKILL.md", name)),
            root: PathBuf::from(format!("skills/{}", name)),
            content: "body".to_string(),
            policy: super::super::metadata::SkillPolicy::default(),
            dependencies: vec![],
            tags: vec![],
            version: None,
            enabled: true,
            quarantined: false,
        }
    }

    #[test]
    fn resolves_at_mention() {
        let skills = vec![sample_skill("rust-expert")];
        let mentions = resolve_mentions("ask @rust-expert for help", &skills);
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].name, "rust-expert");
    }

    #[test]
    fn does_not_match_plain_name_token() {
        // A plain skill name as a loose token must NOT trigger implicit
        // loading — only @mention or skill:// should.
        let skills = vec![sample_skill("rust-expert")];
        let mentions = resolve_mentions("rust-expert: review this", &skills);
        assert!(mentions.is_empty());
    }

    #[test]
    fn does_not_match_substring() {
        let skills = vec![sample_skill("rust")];
        let mentions = resolve_mentions("rust-expert: review this", &skills);
        assert!(mentions.is_empty());
    }

    #[test]
    fn resolves_skill_uri() {
        let skills = vec![sample_skill("rust-expert")];
        let mentions = resolve_mentions("load skill://rust-expert", &skills);
        assert_eq!(mentions.len(), 1);
    }

    #[test]
    fn resolves_at_skill_namespace() {
        // `@skill:{name}` is the disambiguated form — useful when the skill
        // name is also a common word. `@skills:{name}` is the accepted plural.
        let skills = vec![sample_skill("rust-expert")];
        let mentions = resolve_mentions("请按 @skill:rust-expert 规范处理", &skills);
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].name, "rust-expert");

        let mentions = resolve_mentions("load @skills:rust-expert now", &skills);
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].name, "rust-expert");
    }

    #[test]
    fn skill_namespace_does_not_match_partial_name() {
        // `@skill:rust` must not match a skill named `rust-expert` (no prefix
        // match), and `@skill:rust-expert` must not match one named `rust`.
        let long = vec![sample_skill("rust-expert")];
        assert!(resolve_mentions("use @skill:rust", &long).is_empty());

        let short = vec![sample_skill("rust")];
        assert!(resolve_mentions("use @skill:rust-expert", &short).is_empty());
    }

    #[test]
    fn resolves_multiple_distinct_skills_via_namespace() {
        let skills = vec![sample_skill("rust-expert"), sample_skill("pdf")];
        let mentions = resolve_mentions("use @skill:rust-expert and @skills:pdf here", &skills);
        assert_eq!(mentions.len(), 2);
    }

    #[test]
    fn parses_escaped_and_code_spans() {
        let skills = vec![sample_skill("rust-expert")];
        // Backslash escaped
        assert!(resolve_mentions(r"use \@skill:rust-expert here", &skills).is_empty());
        // Inline code span
        assert!(resolve_mentions("use `@skill:rust-expert` here", &skills).is_empty());
        assert!(resolve_mentions("use `skill://rust-expert` here", &skills).is_empty());
        // Non-word-boundary
        assert!(resolve_mentions("foo@skill:rust-expert", &skills).is_empty());
        // Valid boundary
        assert_eq!(
            resolve_mentions("(@skill:rust-expert)", &skills).len(),
            1
        );
    }

    #[test]
    fn format_skill_injection_produces_xml_envelope_with_guidance() {
        let skill = sample_skill("rust-expert");
        let formatted = format_skill_injection(&skill, "# Guidelines\nUse Result.");
        assert!(
            formatted.starts_with(
                "<skill name=\"rust-expert\" scope=\"repo\" ref=\"@skill:rust-expert\">"
            )
        );
        assert!(formatted.contains("<system_guidance>"));
        assert!(formatted.contains("The user activated domain skill \"rust-expert\"."));
        assert!(formatted.contains("Skill Root: skills/rust-expert"));
        assert!(formatted.contains("<instructions>\n# Guidelines\nUse Result.\n</instructions>"));
        assert!(formatted.ends_with("</skill>"));
    }

    /// ADR-0288: the envelope carries the canonical `@skill:` address so the
    /// provider reads an asset reference, not the surface spelling the user
    /// happened to type (`@name` / `@skills:` / `skill://`).
    #[test]
    fn skill_envelope_carries_canonical_address() {
        let skill = sample_skill("pdf");
        let formatted = format_skill_injection(&skill, "body");
        assert!(
            formatted.contains("ref=\"@skill:pdf\""),
            "canonical address must travel in ref=: {formatted}"
        );
    }
}
