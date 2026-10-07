//! Skill discovery across project, user, configured, and remote sources.

use super::SkillsConfig;
use super::metadata::{Skill, SkillScope, parse_skill_metadata};
use super::remote::{cached_remote_roots, fetch_remote_repo};
use nuo_wire::WorkspaceTrustState;
use nuo_persistence::paths;
use nuo_persistence::workspace_security::WorkspaceSecurityStore;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Project-local skills directories (relative to project root).
const PROJECT_NUO_SKILLS_DIR: &str = ".nuo/skills";
const PROJECT_GENERIC_SKILLS_DIR: &str = "skills";
const MAX_SCAN_DEPTH: usize = 8;

/// A project-local ([`SkillScope::Repo`]) skill that overrode a same-named
/// skill from a lower-priority scope during discovery.
///
/// Surfaced to the user as a warning notice by the runtime: a cloned or
/// vendored repo can shadow a user's own skill merely by reusing its name,
/// and a silent override would make that injection invisible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowedSkill {
    /// The skill name claimed by both scopes.
    pub name: String,
    /// The scope that lost (the skill the user would otherwise have gotten).
    pub overridden_scope: SkillScope,
    /// `SKILL.md` path of the winning project-local skill.
    pub winner_source: PathBuf,
}

/// Result of scanning every configured skill source.
#[derive(Debug, Default, Clone)]
pub struct DiscoveryResult {
    pub skills: Vec<Skill>,
    pub errors: Vec<String>,
    /// Project-local skills that shadowed a same-named lower-scope skill in
    /// this scan. Empty unless the workspace skills-domain state admits the
    /// project-local sources.
    pub shadowed: Vec<ShadowedSkill>,
}

/// Discover all skills using the provided configuration.
///
/// Sources are scanned from lowest to highest priority so that higher-priority
/// skills override lower-priority skills with the same name.
///
/// Project-local sources (`.nuo/skills`, `skills/`) are discovered transparently in
/// all trust states (ADR-0165). While the workspace's skills-domain state is
/// [`WorkspaceTrustState::Quarantined`], project skills are marked `quarantined: true`
/// and `enabled: false`, surfaced in TUI and CLI with actionable `/trust skills` guidance
/// without being injected into model context.
pub async fn discover_all(config: &SkillsConfig) -> DiscoveryResult {
    let trust_state = WorkspaceSecurityStore::load()
        .snapshot(&resolve_project_root(config))
        .skills;
    discover_all_with_trust_state(config, trust_state).await
}

/// Discovery with an explicit skills-domain trust state. Production code uses
/// [`discover_all`], which resolves the live state from workspace security;
/// this seam lets tests cover every admission state without mutating user
/// state.
pub async fn discover_all_with_trust_state(
    config: &SkillsConfig,
    trust_state: WorkspaceTrustState,
) -> DiscoveryResult {
    let mut result = DiscoveryResult::default();
    // name -> position in `result.skills`. Scanning runs lowest- to
    // highest-priority; `upsert_skill` makes the last claimant of a name win
    // while preserving the first-seen position for stable catalog ordering.
    let mut index: HashMap<String, usize> = HashMap::new();

    for source in skill_sources(config, trust_state).await {
        match source {
            SkillSource::Local {
                root,
                scope,
                quarantined,
            } => {
                discover_local_skills(&root, scope, quarantined, config, &mut index, &mut result);
            }
            SkillSource::Remote { roots } => {
                for root in roots {
                    discover_local_skills(
                        &root,
                        SkillScope::Remote,
                        false,
                        config,
                        &mut index,
                        &mut result,
                    );
                }
            }
        }
    }

    result
}

enum SkillSource {
    Local {
        root: PathBuf,
        scope: SkillScope,
        quarantined: bool,
    },
    Remote {
        roots: Vec<PathBuf>,
    },
}

async fn skill_sources(
    config: &SkillsConfig,
    trust_state: WorkspaceTrustState,
) -> Vec<SkillSource> {
    let mut sources: Vec<SkillSource> = Vec::new();
    let dirs = paths::get();

    // 1. Remote skill repositories (lowest priority).
    //    When a fetch fails (network down, server error), fall back to the
    //    last successful download's cache so a transient outage never silently
    //    removes skills — the cache-as-fallback pattern every remote catalog
    //    in nuo uses.
    for url in &config.urls {
        match fetch_remote_repo(url).await {
            Ok(roots) if !roots.is_empty() => {
                sources.push(SkillSource::Remote { roots });
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(
                    "failed to fetch remote skill repo '{}': {}; falling back to cache",
                    url,
                    e
                );
                let cached = cached_remote_roots(url);
                if !cached.is_empty() {
                    tracing::info!("using {} cached skills from '{}'", cached.len(), url);
                    sources.push(SkillSource::Remote { roots: cached });
                }
            }
        }
    }

    // 2. User-global nuo skills (XDG; the canonical user location).
    sources.push(SkillSource::Local {
        root: dirs.user_skills_dir(),
        scope: SkillScope::User,
        quarantined: false,
    });

    // 2.5 Role-scoped skills ($XDG_DATA_HOME/nuo/roles/<name>/skills/, ADR-0253).
    if let Some(role) = &config.role {
        sources.push(SkillSource::Local {
            root: dirs.role_skills_dir(role),
            scope: SkillScope::Role,
            quarantined: false,
        });
    }

    // 3. Configured extra paths.
    for path in &config.paths {
        let expanded = expand_tilde(path);
        sources.push(SkillSource::Local {
            root: expanded,
            scope: SkillScope::Extra,
            quarantined: false,
        });
    }

    // 4. Project-local skills (highest priority).
    //    Discovered transparently in all trust states; flagged as quarantined when untrusted (ADR-0165).
    let project_root = resolve_project_root(config);
    let is_quarantined = !trust_state.is_trusted();
    sources.push(SkillSource::Local {
        root: project_root.join(PROJECT_NUO_SKILLS_DIR),
        scope: SkillScope::Repo,
        quarantined: is_quarantined,
    });
    sources.push(SkillSource::Local {
        root: project_root.join(PROJECT_GENERIC_SKILLS_DIR),
        scope: SkillScope::Repo,
        quarantined: is_quarantined,
    });

    sources
}

/// The project root project-local sources resolve from: the config-pinned
/// root when the session bootstrap designated one, else the nearest marker
/// directory above the process cwd.
fn resolve_project_root(config: &SkillsConfig) -> PathBuf {
    match &config.project_root {
        Some(root) => root.clone(),
        None => find_project_root(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
    }
}

/// Whether the project tree declares any project-local skills. Used to word
/// extension-security notices without running a full
/// scan; deliberately cheap and purely local.
pub fn project_skills_present(project_root: &Path) -> bool {
    [PROJECT_NUO_SKILLS_DIR, PROJECT_GENERIC_SKILLS_DIR]
        .iter()
        .any(|dir| {
            walkdir::WalkDir::new(project_root.join(dir))
                .max_depth(MAX_SCAN_DEPTH)
                .follow_links(true)
                .into_iter()
                .filter_map(|entry| entry.ok())
                .any(|entry| entry.file_type().is_file() && entry.file_name() == "SKILL.md")
        })
}

/// Collect all directory roots that may hold discoverable skills for reactive file watching.
pub fn discoverable_skill_directories(config: &SkillsConfig) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let paths_helper = paths::get();

    // 1. User-global nuo skills ($XDG_DATA_HOME/nuo/skills)
    dirs.push(paths_helper.user_skills_dir());

    // 1.5 Role-scoped skills ($XDG_DATA_HOME/nuo/roles/<name>/skills/, ADR-0253)
    if let Some(role) = &config.role {
        dirs.push(paths_helper.role_skills_dir(role));
    }

    // 2. Configured extra paths
    for path in &config.paths {
        dirs.push(expand_tilde(path));
    }

    // 3. Project-local roots (.nuo/skills, skills)
    let project_root = resolve_project_root(config);
    dirs.push(project_root.join(PROJECT_NUO_SKILLS_DIR));
    dirs.push(project_root.join(PROJECT_GENERIC_SKILLS_DIR));

    // 4. Remote cache root ($XDG_CACHE_HOME/nuo/skills/remote)
    dirs.push(paths_helper.remote_skills_cache());

    dirs
}

/// Insert a skill, or — when a higher-priority source already claimed the
/// same name — override the earlier entry in place. Scanning runs from lowest
/// to highest priority, so the last source to claim a name wins, while the
/// first-seen position is preserved for stable catalog ordering.
///
/// Returns a [`ShadowedSkill`] record when the winner is project-local
/// ([`SkillScope::Repo`]) and the loser was not: that is the injection-visible
/// case (a repo silently overriding the user's own skill). Same-scope and
/// lower-scope-wins replacements are routine priority resolution and are not
/// reported.
fn upsert_skill(
    skills: &mut Vec<Skill>,
    index: &mut HashMap<String, usize>,
    skill: Skill,
) -> Option<ShadowedSkill> {
    match index.get(&skill.name).copied() {
        Some(i) => {
            let shadowed = (skill.scope == SkillScope::Repo && skills[i].scope != SkillScope::Repo)
                .then(|| ShadowedSkill {
                    name: skill.name.clone(),
                    overridden_scope: skills[i].scope,
                    winner_source: skill.source.clone(),
                });
            skills[i] = skill;
            shadowed
        }
        None => {
            index.insert(skill.name.clone(), skills.len());
            skills.push(skill);
            None
        }
    }
}

fn discover_local_skills(
    root: &Path,
    scope: SkillScope,
    quarantined: bool,
    config: &SkillsConfig,
    index: &mut HashMap<String, usize>,
    result: &mut DiscoveryResult,
) {
    if !root.is_dir() {
        return;
    }

    for entry in walkdir::WalkDir::new(root)
        .max_depth(MAX_SCAN_DEPTH)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        // Skip hidden subdirectories by checking the relative path.
        if is_inside_hidden_dir(root, entry.path()) {
            continue;
        }
        if entry
            .file_name()
            .to_str()
            .map(|n| n == "SKILL.md")
            .unwrap_or(false)
        {
            let source = entry.path();
            let skill_root = source.parent().unwrap_or(root).to_path_buf();
            let enabled = !quarantined;
            match parse_skill_metadata(source, &skill_root, scope, enabled) {
                Ok(mut skill) => {
                    skill.quarantined = quarantined;
                    if config.is_disabled(&skill.name) || quarantined {
                        skill.enabled = false;
                    }
                    if let Some(shadowed) = upsert_skill(&mut result.skills, index, skill) {
                        result.shadowed.push(shadowed);
                    }
                }
                Err(e) => result.errors.push(e),
            }
        }
    }
}

fn is_inside_hidden_dir(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    relative
        .ancestors()
        .filter(|p| !p.as_os_str().is_empty())
        .any(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with('.'))
                .unwrap_or(false)
        })
}

/// Find the project root by walking upward from `start` looking for common
/// markers. Delegates to canonical [`paths::find_project_root`].
fn find_project_root(start: &Path) -> PathBuf {
    paths::find_project_root(start)
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        dirs::home_dir()
            .map(|h| h.join(rest))
            .unwrap_or_else(|| PathBuf::from(path))
    } else {
        PathBuf::from(path)
    }
}

#[cfg(test)]
mod tests {
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
    /// ADR-0096) discovers project-local skills from that root, not from the
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

        let config = nuo_wire::SkillsConfig {
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
        let unpinned = nuo_wire::SkillsConfig::default();
        let result = discover_all_with_trust_state(&unpinned, WorkspaceTrustState::Trusted).await;
        assert!(
            !result.skills.iter().any(|skill| skill.name == "pinned"),
            "unpinned discovery must not reach into an unrelated directory"
        );
    }

    /// Repo-scope sources are discovered transparently in all trust states, but
    /// flagged as quarantined: true and enabled: false when untrusted (ADR-0165).
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

        let config = nuo_wire::SkillsConfig {
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

        let config = nuo_wire::SkillsConfig {
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
}
