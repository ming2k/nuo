//! nuo configuration initialization.
//!
//! `init_nuo_config` materializes a `.nuo/` configuration tree in a
//! directory (skills, commands, agents) for the `/init` slash command.

use std::path::{Path, PathBuf};

/// Repository-owned instruction files with scope frontmatter.
///
/// Every `.md` under this directory is one instruction entry. Frontmatter
/// selects its scope (`role`, `provider`, `connection`, `model`); no
/// frontmatter means project-global. The directory is part of the Instructions
/// trust domain digest, so its contents are attested before this loader runs.
pub const PROJECT_INSTRUCTIONS_DIR: &str = nuo_host::PROJECT_INSTRUCTIONS_DIR;

/// The files whose contents are injected into the system prompt as project
/// rules.
///
/// Bound to [`nuo_host::PROJECT_RULE_FILES`] so this loader and the Instructions
/// trust digest can never disagree about *which* files are governed by the
/// domain. They used to be two independent lists, and the loader read one file
/// more than the digest covered.
const PROJECT_RULE_FILES: &[&str] = nuo_host::PROJECT_RULE_FILES;

/// Load the trusted project instruction files into one explicitly delimited
/// system-prompt section. Slash-command templates share the Rules trust domain
/// but are discovered by `commands` rather than copied into the prompt.
///
/// Every file read here is covered by the Instructions domain digest, so this
/// function is only ever called after that domain is trusted — the two are one
/// trust boundary, not two.
pub fn load_project_rules(base: &Path) -> Result<String, String> {
    let mut sections = Vec::new();
    for relative in PROJECT_RULE_FILES {
        let path = base.join(relative);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "Failed to read project rule file '{}': {error}",
                    path.display()
                ));
            }
        };
        sections.push(format!(
            "[BEGIN PROJECT RULES: {relative}]\n{text}\n[END PROJECT RULES: {relative}]"
        ));
    }
    Ok(sections.join("\n\n"))
}

/// Load the scoped project instructions under `.nuo/instructions`.
///
/// Each Markdown file is one entry. Optional YAML frontmatter selects its
/// scope; an absent frontmatter block means project-global. Files are returned
/// in repository-relative path order, which is the deterministic declaration
/// order the composer uses after scope ranking.
///
/// This loader only runs behind the Instructions trust domain, whose digest
/// covers this directory (`nuo_host::PROJECT_INSTRUCTION_PATHS`). Symlinks are
/// rejected here as well as in the digest: trust must not be bypassable by
/// swapping a file for a link between attestation and load.
pub fn load_project_instructions(
    base: &Path,
) -> Result<Vec<nuo_protocol::ScopedInstruction>, String> {
    let root = base.join(PROJECT_INSTRUCTIONS_DIR);
    let mut files = Vec::new();
    collect_instruction_files(&root, &mut files)?;
    files.sort();

    let mut instructions = Vec::with_capacity(files.len());
    for path in files {
        let relative = path
            .strip_prefix(base)
            .map_err(|error| {
                format!(
                    "project instruction '{}' escaped the workspace root: {error}",
                    path.display()
                )
            })?
            .to_str()
            .ok_or_else(|| {
                format!(
                    "project instruction path '{}' is not valid UTF-8",
                    path.display()
                )
            })?
            .replace('\\', "/");
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("failed to read '{relative}': {error}"))?;
        let (frontmatter, body) = split_frontmatter(&text)
            .map_err(|error| format!("invalid frontmatter in '{relative}': {error}"))?;
        let selector = match frontmatter {
            Some(yaml) => serde_yaml::from_str::<nuo_protocol::InstructionSelector>(yaml)
                .map_err(|error| format!("invalid frontmatter in '{relative}': {error}"))?,
            None => nuo_protocol::InstructionSelector::default(),
        };
        selector
            .validate()
            .map_err(|error| format!("invalid selector in '{relative}': {error}"))?;
        let body = body.trim();
        if body.is_empty() {
            return Err(format!("project instruction '{relative}' has no body"));
        }
        instructions.push(nuo_protocol::ScopedInstruction {
            id: relative,
            source: nuo_protocol::InstructionSource::Project,
            ordinal: instructions.len(),
            selector,
            text: body.to_string(),
        });
    }
    Ok(instructions)
}

fn collect_instruction_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let meta = match std::fs::symlink_metadata(dir) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!("cannot inspect '{}': {error}", dir.display()));
        }
    };
    if meta.file_type().is_symlink() {
        return Err(format!(
            "project instruction path '{}' is a symlink; symlinked content cannot be trusted",
            dir.display()
        ));
    }
    if meta.is_file() {
        if dir.extension().and_then(|extension| extension.to_str()) == Some("md") {
            out.push(dir.to_path_buf());
        }
        return Ok(());
    }
    if !meta.is_dir() {
        return Ok(());
    }
    let mut entries = std::fs::read_dir(dir)
        .map_err(|error| format!("cannot read '{}': {error}", dir.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("cannot read '{}': {error}", dir.display()))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        collect_instruction_files(&entry.path(), out)?;
    }
    Ok(())
}

/// Split an optional YAML frontmatter block from the Markdown body.
///
/// A document without an opening `---` line has no frontmatter. An opened
/// block must close with a line that is exactly `---`; otherwise the author
/// made a structural mistake and the file is rejected rather than silently
/// treated as body text.
fn split_frontmatter(text: &str) -> Result<(Option<&str>, &str), String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if !text.starts_with("---") {
        return Ok((None, text));
    }
    let after_open = text[3..]
        .strip_prefix("\r\n")
        .or_else(|| text[3..].strip_prefix('\n'));
    let Some(rest) = after_open else {
        return Ok((None, text));
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            return Ok((Some(&rest[..offset]), &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    Err("unterminated frontmatter (missing closing `---`)".to_string())
}

/// Materialize a `.nuo/` tree. Returns the list of newly created relative
/// paths (existing files are left untouched and not reported).
pub fn init_nuo_config(base: &Path) -> Result<Vec<String>, String> {
    let mut created = Vec::new();
    let dirs = ["skills", "agents", "instructions"];
    for dir in dirs {
        let path = base.join(".nuo").join(dir);
        if !path.exists() {
            std::fs::create_dir_all(&path)
                .map_err(|e| format!("Failed to create '{}': {}", path.display(), e))?;
            created.push(format!(".nuo/{}/.keep", dir));
            std::fs::write(path.join(".keep"), "")
                .map_err(|e| format!("Failed to write keep file: {}", e))?;
        }
    }

    // Drop a starter skill template so users can see the SKILL.md format.
    let example_skill = base.join(".nuo/skills/example/SKILL.md");
    if !example_skill.exists() {
        if let Some(parent) = example_skill.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create '{}': {}", parent.display(), e))?;
        }
        std::fs::write(&example_skill, example_skill_template())
            .map_err(|e| format!("Failed to write '{}': {}", example_skill.display(), e))?;
        created.push(".nuo/skills/example/SKILL.md".to_string());
    }

    let agents_md = base.join("AGENTS.md");
    if !agents_md.exists() {
        std::fs::write(&agents_md, agents_md_template(base))
            .map_err(|e| format!("Failed to write AGENTS.md: {}", e))?;
        created.push("AGENTS.md".to_string());
    }

    let gitignore = base.join(".gitignore");
    if !gitignore.exists() {
        std::fs::write(&gitignore, nuo_gitignore())
            .map_err(|e| format!("Failed to write .gitignore: {}", e))?;
        created.push(".gitignore".to_string());
    }

    Ok(created)
}

fn example_skill_template() -> &'static str {
    "---\n\
     name: example\n\
     description: An example skill showing the frontmatter format.\n\
     short-description: Example skill\n\
     ---\n\
     \n\
     # Example Skill\n\
     \n\
     Edit this file or add more `.nuo/skills/<name>/SKILL.md` files to teach\n\
     nuo domain-specific conventions, build steps, or review checklists.\n"
}

fn agents_md_template(base: &Path) -> String {
    let project_name = base
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("this project");
    format!(
        "# {name} — Agent Guide\n\n\
         Background, architecture, and conventions coding agents need to work\n\
         effectively in this repository. Fill in the sections below as the\n\
         project matures.\n\n\
         ## Overview\n\n\
         Describe what `{name}` does and its high-level architecture.\n\n\
         ## Build & Test\n\n\
         ```\n\
         # build\n\
         # test\n\
         # lint\n\
         ```\n\n\
         ## Conventions\n\n\
         - Coding style and patterns\n\
         - Where new code should go\n\
         - Anything an agent must know before editing\n",
        name = project_name
    )
}

fn nuo_gitignore() -> &'static str {
    "# nuo\n.nuo/session.json\n.nuo/sessions/\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_nuo_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let first = init_nuo_config(dir).unwrap();
        assert!(first.iter().any(|p| p == "AGENTS.md"));
        let second = init_nuo_config(dir).unwrap();
        assert!(second.is_empty());
    }

    #[test]
    fn project_rules_are_delimited_by_source() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("AGENTS.md"), "Run nextest.").unwrap();
        std::fs::write(temp.path().join(".cursorrules"), "Keep changes small.").unwrap();
        let rules = load_project_rules(temp.path()).unwrap();
        assert!(rules.contains("[BEGIN PROJECT RULES: AGENTS.md]"));
        assert!(rules.contains("Run nextest."));
        assert!(rules.contains("[BEGIN PROJECT RULES: .cursorrules]"));
    }

    fn write_instruction(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn scoped_instructions_parse_frontmatter_and_body() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write_instruction(root, ".nuo/instructions/global.md", "Global rule.");
        write_instruction(
            root,
            ".nuo/instructions/reviewer.md",
            "---\nrole: reviewer\nmodel: \"*deepseek*\"\n---\nReview rule.",
        );
        let instructions = load_project_instructions(root).unwrap();
        assert_eq!(instructions.len(), 2);
        // Path order is the declaration order.
        assert_eq!(instructions[0].id, ".nuo/instructions/global.md");
        assert!(instructions[0].selector.is_global());
        assert_eq!(instructions[0].text, "Global rule.");
        assert_eq!(instructions[1].id, ".nuo/instructions/reviewer.md");
        assert_eq!(instructions[1].selector.role.as_deref(), Some("reviewer"));
        assert_eq!(
            instructions[1].selector.model.as_deref(),
            Some("*deepseek*")
        );
        assert_eq!(instructions[1].text, "Review rule.");
    }

    #[test]
    fn instruction_frontmatter_rejects_unknown_keys() {
        let temp = tempfile::tempdir().unwrap();
        write_instruction(
            temp.path(),
            ".nuo/instructions/bad.md",
            "---\nscope: reviewer\n---\nBody.",
        );
        let error = load_project_instructions(temp.path()).unwrap_err();
        assert!(error.contains("bad.md"), "{error}");
    }

    #[test]
    fn unterminated_frontmatter_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        write_instruction(
            temp.path(),
            ".nuo/instructions/bad.md",
            "---\nrole: reviewer\nBody without a closer.",
        );
        let error = load_project_instructions(temp.path()).unwrap_err();
        assert!(error.contains("unterminated frontmatter"), "{error}");
    }

    #[test]
    fn empty_instruction_body_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        write_instruction(
            temp.path(),
            ".nuo/instructions/empty.md",
            "---\nrole: reviewer\n---\n   \n",
        );
        let error = load_project_instructions(temp.path()).unwrap_err();
        assert!(error.contains("no body"), "{error}");
    }

    #[test]
    fn non_markdown_instruction_files_are_ignored() {
        let temp = tempfile::tempdir().unwrap();
        write_instruction(
            temp.path(),
            ".nuo/instructions/README.txt",
            "not an instruction",
        );
        assert!(load_project_instructions(temp.path()).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_instruction_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write_instruction(root, "shared.md", "Shared.");
        std::fs::create_dir_all(root.join(".nuo/instructions")).unwrap();
        std::os::unix::fs::symlink(
            root.join("shared.md"),
            root.join(".nuo/instructions/link.md"),
        )
        .unwrap();
        let error = load_project_instructions(root).unwrap_err();
        assert!(error.contains("symlink"), "{error}");
    }
}
