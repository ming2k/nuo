use super::*;

#[test]
fn project_root_detects_git() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let nested = root.join("a").join("b");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap();

    assert_eq!(find_project_root(&nested), root);
}

#[test]
fn project_root_falls_back_to_start() {
    let temp = tempfile::tempdir().unwrap();
    assert_eq!(find_project_root(temp.path()), temp.path());
}

/// Regression (the "wrong workspace" bug): a config whose `project_root`
/// is pinned (the session bootstrap does this under the unified server,
/// ) discovers project-local skills from that root, not from the
/// process cwd — which under the server belongs to a different project
/// than the session invoking discovery.
///
/// Uses the explicit-state seam because `discover_all` consults persisted
/// workspace security, while this temporary workspace has no grant.
#[tokio::test]
async fn pinned_project_root_scopes_project_local_skills() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let skill_dir = root.join(".nuo/skills/pinned");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: pinned\ndescription: pinned to the session project\n---\n# Pinned\n",
    )
    .unwrap();

    let config = nuo_protocol::SkillsConfig {
        project_root: Some(root.to_path_buf()),
        ..Default::default()
    };
    let result = discover_all_with_trust_state(&config, WorkspaceTrustState::Trusted).await;
    assert!(
        result.skills.iter().any(|skill| skill.name == "pinned"),
        "project-local skill must be discovered from the pinned root"
    );

    // Without a pinned root the same config discovers nothing here: the
    // process cwd (the test binary's) has no `.nuo/skills/pinned`.
    let unpinned = nuo_protocol::SkillsConfig::default();
    let result = discover_all_with_trust_state(&unpinned, WorkspaceTrustState::Trusted).await;
    assert!(
        !result.skills.iter().any(|skill| skill.name == "pinned"),
        "unpinned discovery must not reach into an unrelated directory"
    );
}

/// Repo-scope sources are discovered transparently in all trust states, but
/// flagged as quarantined: true and enabled: false when untrusted.
#[tokio::test]
async fn quarantined_skills_discovered_as_disabled_and_quarantined() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    for dir in [".nuo/skills/evil", "skills/evil2"] {
        let skill_dir = root.join(dir);
        std::fs::create_dir_all(&skill_dir).unwrap();
        let name = skill_dir.file_name().unwrap().to_string_lossy().to_string();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: planted\n---\n# Planted\n"),
        )
        .unwrap();
    }

    let config = nuo_protocol::SkillsConfig {
        project_root: Some(root.to_path_buf()),
        ..Default::default()
    };

    let result = discover_all_with_trust_state(&config, WorkspaceTrustState::Quarantined).await;
    let evil = result
        .skills
        .iter()
        .find(|s| s.name == "evil")
        .expect("evil should be discovered");
    assert!(
        evil.quarantined,
        "untrusted repo skill must be marked quarantined"
    );
    assert!(!evil.enabled, "quarantined skill must be disabled");
    assert!(
        !evil.allows_implicit_invocation(),
        "quarantined skill must not allow implicit invocation"
    );

    let result = discover_all_with_trust_state(&config, WorkspaceTrustState::Trusted).await;
    let evil_trusted = result
        .skills
        .iter()
        .find(|s| s.name == "evil")
        .expect("evil should be discovered");
    assert!(
        !evil_trusted.quarantined,
        "trusted repo skill must not be quarantined"
    );
    assert!(evil_trusted.enabled, "trusted repo skill must be enabled");
}

#[tokio::test]
async fn role_scoped_skills_discovered_with_role_scope() {
    let temp = tempfile::tempdir().unwrap();
    let role_dir = temp
        .path()
        .join("roles")
        .join("philosophist")
        .join("skills")
        .join("socratic");
    std::fs::create_dir_all(&role_dir).unwrap();
    std::fs::write(
        role_dir.join("SKILL.md"),
        "---\nname: socratic\ndescription: Socratic questioning\n---\nPrompt here",
    )
    .unwrap();

    let config = nuo_protocol::SkillsConfig {
        role: Some("philosophist".to_string()),
        paths: vec![role_dir.to_str().unwrap().to_string()],
        ..Default::default()
    };
    let result = discover_all_with_trust_state(&config, WorkspaceTrustState::Trusted).await;
    let skill = result
        .skills
        .iter()
        .find(|s| s.name == "socratic")
        .expect("socratic skill should be discovered");
    assert_eq!(skill.name, "socratic");
}

#[test]
fn expand_tilde_resolves_home() {
    let home = dirs::home_dir().unwrap();
    assert_eq!(expand_tilde("~/foo"), home.join("foo"));
}

#[test]
fn higher_priority_source_overrides_lower_on_name_collision() {
    // Scanning order encodes priority (lowest first). A skill with the same
    // name in a later-scanned (higher-priority) source must override the
    // earlier one, while keeping the first-seen catalog position.
    let low = std::env::temp_dir().join(format!("nuo-skill-{}", uuid::Uuid::new_v4()));
    let high = std::env::temp_dir().join(format!("nuo-skill-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(low.join("shared")).unwrap();
    std::fs::create_dir_all(high.join("shared")).unwrap();
    std::fs::write(
        low.join("shared").join("SKILL.md"),
        "---\nname: shared\ndescription: low\n---\nlow body",
    )
    .unwrap();
    std::fs::write(
        high.join("shared").join("SKILL.md"),
        "---\nname: shared\ndescription: high\n---\nhigh body",
    )
    .unwrap();

    let config = SkillsConfig::default();
    let mut result = DiscoveryResult::default();
    let mut index: HashMap<String, usize> = HashMap::new();
    // User scope first (lower priority), then Repo (higher priority).
    discover_local_skills(
        &low,
        SkillScope::User,
        false,
        &config,
        &mut index,
        &mut result,
    );
    discover_local_skills(
        &high,
        SkillScope::Repo,
        false,
        &config,
        &mut index,
        &mut result,
    );

    assert_eq!(result.skills.len(), 1, "collision should not duplicate");
    let skill = &result.skills[0];
    assert_eq!(skill.scope, SkillScope::Repo, "higher-priority source wins");
    assert_eq!(skill.description, "high");
    // Body is loaded lazily, so it is empty right after discovery...
    assert!(
        skill.content.is_empty(),
        "body is not read at discovery time"
    );
    // ...and resolves on demand from the winning source.
    assert_eq!(skill.load_body().unwrap(), "high body");

    let _ = std::fs::remove_dir_all(&low);
    let _ = std::fs::remove_dir_all(&high);
}

#[test]
fn disabled_flag_survives_override() {
    // A higher-priority source still honours [skills] disabled for its name.
    let low = std::env::temp_dir().join(format!("nuo-skill-{}", uuid::Uuid::new_v4()));
    let high = std::env::temp_dir().join(format!("nuo-skill-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(low.join("x")).unwrap();
    std::fs::create_dir_all(high.join("x")).unwrap();
    std::fs::write(low.join("x").join("SKILL.md"), "---\nname: x\n---\nlow").unwrap();
    std::fs::write(high.join("x").join("SKILL.md"), "---\nname: x\n---\nhigh").unwrap();

    let config = SkillsConfig {
        disabled: vec!["x".to_string()],
        ..SkillsConfig::default()
    };
    let mut result = DiscoveryResult::default();
    let mut index: HashMap<String, usize> = HashMap::new();
    discover_local_skills(
        &low,
        SkillScope::User,
        false,
        &config,
        &mut index,
        &mut result,
    );
    discover_local_skills(
        &high,
        SkillScope::Repo,
        false,
        &config,
        &mut index,
        &mut result,
    );

    assert_eq!(result.skills.len(), 1);
    assert!(
        !result.skills[0].enabled,
        "disabled config applies to the overriding skill"
    );

    let _ = std::fs::remove_dir_all(&low);
    let _ = std::fs::remove_dir_all(&high);
}

#[test]
fn repo_skill_shadowing_user_skill_is_recorded_exactly_once() {
    // A project-local (Repo) skill that claims a name already held by a
    // user-scope skill wins by priority — and must leave exactly one
    // shadow record so the runtime can warn about the silent override.
    let user = std::env::temp_dir().join(format!("nuo-skill-{}", uuid::Uuid::new_v4()));
    let repo = std::env::temp_dir().join(format!("nuo-skill-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(user.join("shared")).unwrap();
    std::fs::create_dir_all(repo.join("shared")).unwrap();
    std::fs::write(
        user.join("shared").join("SKILL.md"),
        "---\nname: shared\ndescription: user\n---\nuser body",
    )
    .unwrap();
    std::fs::write(
        repo.join("shared").join("SKILL.md"),
        "---\nname: shared\ndescription: repo\n---\nrepo body",
    )
    .unwrap();

    let config = SkillsConfig::default();
    let mut result = DiscoveryResult::default();
    let mut index: HashMap<String, usize> = HashMap::new();
    discover_local_skills(
        &user,
        SkillScope::User,
        false,
        &config,
        &mut index,
        &mut result,
    );
    discover_local_skills(
        &repo,
        SkillScope::Repo,
        false,
        &config,
        &mut index,
        &mut result,
    );

    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.skills[0].scope, SkillScope::Repo);
    assert_eq!(
        result.shadowed.len(),
        1,
        "exactly one shadow record per shadowed name"
    );
    let shadow = &result.shadowed[0];
    assert_eq!(shadow.name, "shared");
    assert_eq!(shadow.overridden_scope, SkillScope::User);
    assert!(shadow.winner_source.ends_with("SKILL.md"));

    let _ = std::fs::remove_dir_all(&user);
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn same_scope_or_lower_scope_overrides_are_not_shadow_records() {
    // Repo-over-Repo (two project dirs) and User-over-Remote are routine
    // priority resolution within one trust domain — no warning.
    let low = std::env::temp_dir().join(format!("nuo-skill-{}", uuid::Uuid::new_v4()));
    let high = std::env::temp_dir().join(format!("nuo-skill-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(low.join("x")).unwrap();
    std::fs::create_dir_all(high.join("x")).unwrap();
    std::fs::write(low.join("x").join("SKILL.md"), "---\nname: x\n---\nlow").unwrap();
    std::fs::write(high.join("x").join("SKILL.md"), "---\nname: x\n---\nhigh").unwrap();

    let config = SkillsConfig::default();
    let mut result = DiscoveryResult::default();
    let mut index: HashMap<String, usize> = HashMap::new();
    discover_local_skills(
        &low,
        SkillScope::Repo,
        false,
        &config,
        &mut index,
        &mut result,
    );
    discover_local_skills(
        &high,
        SkillScope::Repo,
        false,
        &config,
        &mut index,
        &mut result,
    );
    assert_eq!(result.skills.len(), 1);
    assert!(
        result.shadowed.is_empty(),
        "repo-over-repo is not a user-visible shadow"
    );

    let _ = std::fs::remove_dir_all(&low);
    let _ = std::fs::remove_dir_all(&high);
}
