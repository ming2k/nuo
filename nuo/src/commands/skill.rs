use crate::cli::SkillAction;
use nuo_persistence::config::Config;
use nuo_persistence::paths;
use std::path::PathBuf;

pub async fn run(action: SkillAction) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        SkillAction::List => list().await,
        SkillAction::Show { name } => show(&name).await,
        SkillAction::Info { name } => info(&name).await,
        SkillAction::Init { name, user } => init(&name, user).await,
    }
}

async fn list() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::load();
    let result = nuo_harness::skills::discovery::discover_all(&config.skills).await;

    if result.skills.is_empty() {
        println!("No skills discovered.");
        println!(
            "Tip: Scaffold a new skill with `nuo skill init <name>` or place files in .nuo/skills/<name>/SKILL.md."
        );
        return Ok(());
    }

    println!(
        "{:<22} {:<10} {:<14} {:<10} Description",
        "Skill Name", "Scope", "Status", "Version"
    );
    println!(
        "{:-<22} {:-<10} {:-<14} {:-<10} {:-<30}",
        "", "", "", "", ""
    );

    let mut has_quarantined = false;

    for skill in &result.skills {
        let scope = format!("{:?}", skill.scope);
        let version = skill.version.as_deref().unwrap_or("–");
        let status = if skill.quarantined {
            has_quarantined = true;
            "Quarantined"
        } else if skill.enabled {
            "Enabled"
        } else {
            "Disabled"
        };
        let desc = skill
            .short_description
            .as_deref()
            .unwrap_or(skill.description.as_str());
        let short_desc = if desc.chars().count() > 45 {
            format!("{}...", desc.chars().take(42).collect::<String>())
        } else {
            desc.to_string()
        };
        println!(
            "{:<22} {:<10} {:<14} {:<10} {}",
            skill.name, scope, status, version, short_desc
        );
    }

    if has_quarantined {
        println!(
            "\nNote: Quarantined skills belong to untrusted workspace roots. Run `/trust skills` or `/trust` to authorize."
        );
    }

    if !result.errors.is_empty() {
        eprintln!("\nWarnings during discovery:");
        for err in &result.errors {
            eprintln!("  • {err}");
        }
    }
    Ok(())
}

async fn show(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::load();
    let result = nuo_harness::skills::discovery::discover_all(&config.skills).await;

    let skill = result.skills.iter().find(|s| s.name == name);
    match skill {
        Some(s) => {
            let body = s
                .load_body()
                .map_err(|e| format!("Failed to read skill body: {e}"))?;
            println!("# Skill: {} ({:?})\n", s.name, s.scope);
            if s.quarantined {
                println!("⚠️  Status: Quarantined (Project Workspace Asset untrusted)\n");
            }
            println!("{}\n", s.description);
            println!("---\n");
            println!("{body}");
            Ok(())
        }
        None => {
            eprintln!(
                "Error: Skill '{name}' not found. Run `nuo skill ls` to see available skills."
            );
            std::process::exit(1);
        }
    }
}

async fn info(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::load();
    let result = nuo_harness::skills::discovery::discover_all(&config.skills).await;

    let skill = result.skills.iter().find(|s| s.name == name);
    match skill {
        Some(s) => {
            let version = s.version.as_deref().unwrap_or("–");
            let status = if s.quarantined {
                "Quarantined (run /trust skills to enable)"
            } else if s.enabled {
                "Enabled"
            } else {
                "Disabled"
            };
            let tags = if s.tags.is_empty() {
                "none".to_string()
            } else {
                s.tags.join(", ")
            };
            println!("Skill:       {}", s.name);
            println!("Scope:       {:?}", s.scope);
            println!("Status:      {}", status);
            println!("Version:     {}", version);
            println!("Tags:        {}", tags);
            println!("Source:      {}", s.source.display());
            println!("Root:        {}", s.root.display());
            println!("Description: {}", s.description);
            Ok(())
        }
        None => {
            eprintln!(
                "Error: Skill '{name}' not found. Run `nuo skill ls` to see available skills."
            );
            std::process::exit(1);
        }
    }
}

async fn init(name: &str, user: bool) -> Result<(), Box<dyn std::error::Error>> {
    let clean_name = name.trim().to_lowercase().replace(' ', "-");
    let target_dir: PathBuf = if user {
        let dirs = paths::get();
        dirs.user_skills_dir().join(&clean_name)
    } else {
        PathBuf::from(".nuo/skills").join(&clean_name)
    };

    if target_dir.exists() {
        eprintln!(
            "Error: Skill directory '{}' already exists.",
            target_dir.display()
        );
        std::process::exit(1);
    }

    std::fs::create_dir_all(target_dir.join("references"))?;
    std::fs::create_dir_all(target_dir.join("scripts"))?;
    std::fs::create_dir_all(target_dir.join("assets"))?;

    let skill_template = format!(
        "---\n\
         name: {clean_name}\n\
         description: Provide a concise description of what this skill enables and specific triggers when to use it.\n\
         short-description: Brief summary\n\
         version: 0.1.0\n\
         tags: []\n\
         policy:\n\
           allow_implicit_invocation: true\n\
         ---\n\n\
         # {clean_name}\n\n\
         Instructions and standard operating procedures for the agent.\n\n\
         ## Workflow\n\n\
         1. Step 1: Analyze requirements\n\
         2. Step 2: Perform execution\n\
         3. Step 3: Verify results\n"
    );

    std::fs::write(target_dir.join("SKILL.md"), skill_template)?;

    println!("✅ Initialized new skill '{clean_name}' at:");
    println!("   {}", target_dir.display());
    println!("\nDirectory structure:");
    println!("   {} /", target_dir.display());
    println!("   ├── SKILL.md");
    println!("   ├── references/");
    println!("   ├── scripts/");
    println!("   └── assets/");
    println!(
        "\nNext steps: Edit {}/SKILL.md and run `nuo skill ls` to verify.",
        target_dir.display()
    );

    Ok(())
}
