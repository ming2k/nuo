//! Skill discovery, metadata, registries, and model-facing tool adapters.
//!
//! Skills are markdown files with YAML frontmatter, stored (in priority order,
//! lowest first) across:
//!   - Remote skill repositories fetched into `$XDG_CACHE_HOME/muta/skills/remote/`.
//!   - User-global skills: `$XDG_DATA_HOME/muta/skills/` (XDG-resolved via
//!     [`nuo_persistence::paths`]).
//!   - Configured extra paths (`[skills] paths = [...]` in `config.toml`).
//!   - Project-local skills: `.muta/skills/<name>/SKILL.md` (highest priority).
//!
//! Frontmatter schema:
//!   ```yaml
//!   ---
//!   name: rust-expert
//!   description: "Use when writing or debugging Rust code"
//!   short-description: "Rust help"
//!   version: "1.0.0"
//!   tags: [rust, cargo]
//!   policy:
//!     allow_implicit_invocation: true
//!   dependencies:
//!     - type: mcp
//!       value: context7
//!   ---
//!   ```

pub mod host;
pub use host::{SkillHost, SkillRoots, SkillTrust};

pub mod discovery;
pub mod metadata;
pub mod remote;
pub mod render;
pub mod tools;

pub use discovery::{
    DiscoveryResult, ShadowedSkill, discover_all, discover_all_with_trust_state,
    discoverable_skill_directories, project_skills_present,
};
pub use metadata::{Skill, SkillDependency, SkillPolicy, SkillScope};
pub use nuo_contracts::SkillsConfig;
pub use render::{format_skill_injection, format_skill_list, list_skill_files, resolve_mentions};
pub use tools::{ListSkillsTool, UseSkillTool};

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

/// Consumer for a scan's newly-observed shadowing events (a project-local
/// skill overriding a same-named lower-scope skill). The runtime installs one
/// to surface a warning notice; the crate itself stays UI-agnostic.
pub type ShadowSink = Arc<dyn Fn(&[ShadowedSkill]) + Send + Sync>;

/// Shadow-notice plumbing shared by every clone of a [`SkillRegistry`]:
/// the installed sink plus the set of skill names already reported this
/// process lifetime (dedupe — a name warns once, not on every hourly rescan).
#[derive(Default)]
struct ShadowState {
    sink: Option<ShadowSink>,
    reported: HashSet<String>,
}

/// Thread-safe in-memory registry of discovered skills.
#[derive(Clone)]
pub struct SkillRegistry {
    inner: Arc<RwLock<RegistryInner>>,
    /// Lazily-populated cache of skill bodies, keyed by skill name. A body is
    /// read from disk (via [`Skill::load_body`]) the first time it is
    /// requested, then reused so repeated `use_skill` / implicit loads in the
    /// same session never re-read the file.
    bodies: Arc<RwLock<HashMap<String, String>>>,
    shadows: Arc<Mutex<ShadowState>>,
}

#[derive(Debug, Default, Clone)]
struct RegistryInner {
    skills: Vec<Skill>,
    errors: Vec<String>,
    /// Project-local skills that shadowed a same-named lower-scope skill in
    /// the most recent scan (see [`discovery::ShadowedSkill`]).
    shadowed: Vec<ShadowedSkill>,
    config: SkillsConfig,
}

impl SkillRegistry {
    /// Create an empty registry with no configuration. `reload()` on such a
    /// registry re-runs discovery with a default (empty) config, so prefer
    /// [`SkillRegistry::empty_with_config`] when the real config is known.
    pub fn empty() -> Self {
        Self {
            inner: Arc::new(RwLock::new(RegistryInner::default())),
            bodies: Arc::new(RwLock::new(HashMap::new())),
            shadows: Arc::new(Mutex::new(ShadowState::default())),
        }
    }

    /// Create an empty registry that remembers `config`. The registry starts
    /// with no discovered skills, but a subsequent [`reload`](Self::reload)
    /// (e.g. from the background refresh loop) re-runs discovery using this
    /// config and populates the registry in place. This is the entry point
    /// for non-blocking startup: hand back an empty registry immediately, let
    /// the background task fill it.
    pub fn empty_with_config(config: &SkillsConfig) -> Self {
        Self {
            inner: Arc::new(RwLock::new(RegistryInner {
                skills: Vec::new(),
                errors: Vec::new(),
                shadowed: Vec::new(),
                config: config.clone(),
            })),
            bodies: Arc::new(RwLock::new(HashMap::new())),
            shadows: Arc::new(Mutex::new(ShadowState::default())),
        }
    }

    /// Discover skills from all configured sources.
    pub async fn load(config: &SkillsConfig) -> Self {
        let result = discover_all(config).await;
        if !result.errors.is_empty() {
            for err in &result.errors {
                tracing::warn!("skill discovery error: {}", err);
            }
        }
        let registry = Self {
            inner: Arc::new(RwLock::new(RegistryInner {
                skills: Vec::new(),
                errors: Vec::new(),
                shadowed: Vec::new(),
                config: config.clone(),
            })),
            bodies: Arc::new(RwLock::new(HashMap::new())),
            shadows: Arc::new(Mutex::new(ShadowState::default())),
        };
        registry.apply_scan(result);
        registry
    }

    /// Rescan all sources using the same configuration that was originally
    /// supplied. If no configuration was stored, performs a default scan.
    pub async fn reload(&self) {
        let config = {
            match self.inner.read() {
                Ok(inner) => inner.config.clone(),
                Err(err) => err.into_inner().config.clone(),
            }
        };
        let result = discover_all(&config).await;
        self.apply_scan(result);
    }

    /// Spawn a reactive, platform-agnostic filesystem watcher (inotify/kqueue/FSEvents/ReadDirectoryChangesW)
    /// on all discoverable skill root paths. Whenever a file change occurs, automatically re-runs discovery.
    pub fn spawn_reactive_watcher(&self) -> Option<tokio::task::JoinHandle<()>> {
        let config = match self.inner.read() {
            Ok(inner) => inner.config.clone(),
            Err(err) => err.into_inner().config.clone(),
        };
        let roots = discoverable_skill_directories(&config);
        let mut watcher =
            match nuo_host::FsWatcher::new(nuo_host::FsWatcher::DEFAULT_DEBOUNCE) {
                Ok(w) => w,
                Err(e) => {
                    tracing::warn!("failed to initialize reactive skills fs watcher: {e}");
                    return None;
                }
            };

        for root in roots {
            let _ = watcher.watch_if_exists(&root, true);
        }

        let mut events_rx = watcher.subscribe();
        let registry = self.clone();

        Some(tokio::spawn(async move {
            let _watcher = watcher;
            // Immediate initial discovery on startup: populates the empty registry (ADR-0165)
            registry.reload().await;

            while let Ok(event) = events_rx.recv().await {
                tracing::debug!(
                    "reactive skill filesystem event: {:?}, reloading...",
                    event.kind
                );
                registry.reload().await;
            }
        }))
    }

    /// Install (or clear) the shadow-notice sink. The sink is called after a
    /// scan with the shadowing events not yet reported this process lifetime
    /// — one call batch per scan, each name reported at most once ever.
    pub fn set_shadow_sink(&self, sink: Option<ShadowSink>) {
        if let Ok(mut shadows) = self.shadows.lock() {
            shadows.sink = sink;
        }
    }

    /// The most recent scan's shadowing events (project-local skills that
    /// overrode a same-named lower-scope skill), reported or not.
    pub fn shadowed(&self) -> Vec<ShadowedSkill> {
        self.lock().guard.shadowed.clone()
    }

    /// Store a finished scan and notify the shadow sink about newly-seen
    /// shadowing. Dedupe is by skill name across the registry's lifetime, so
    /// the hourly background rescan cannot spam a notice the user already saw.
    fn apply_scan(&self, result: DiscoveryResult) {
        if let Ok(mut inner) = self.inner.write() {
            inner.skills = result.skills;
            inner.errors = result.errors;
            inner.shadowed = result.shadowed.clone();
        }
        if let Ok(mut bodies) = self.bodies.write() {
            bodies.clear();
        }
        // Collect the not-yet-reported shadow records and mark them reported,
        // releasing the lock BEFORE invoking the (foreign) sink.
        let notify = self.shadows.lock().ok().and_then(|mut shadows| {
            let fresh: Vec<ShadowedSkill> = result
                .shadowed
                .into_iter()
                .filter(|s| !shadows.reported.contains(&s.name))
                .collect();
            for s in &fresh {
                shadows.reported.insert(s.name.clone());
            }
            match shadows.sink.clone() {
                Some(sink) if !fresh.is_empty() => Some((sink, fresh)),
                _ => None,
            }
        });
        if let Some((sink, fresh)) = notify {
            sink(&fresh);
        }
    }

    /// Acquire a read lock on the registry.
    pub fn lock(&self) -> RegistryGuard<'_> {
        RegistryGuard {
            guard: self.inner.read().unwrap_or_else(|e| e.into_inner()),
        }
    }

    /// Workspace root pinned into this registry's discovery configuration.
    /// Project-scope consumers use it for live content re-attestation.
    pub fn project_root(&self) -> Option<std::path::PathBuf> {
        self.inner
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .config
            .project_root
            .clone()
    }

    /// Replace the registry contents directly, used during tests or when the
    /// caller wants to build a registry without disk discovery.
    pub fn replace(&self, skills: Vec<Skill>) {
        if let Ok(mut inner) = self.inner.write() {
            inner.skills = skills;
            inner.errors.clear();
            inner.shadowed.clear();
        }
        if let Ok(mut bodies) = self.bodies.write() {
            bodies.clear();
        }
    }

    /// Resolve a skill's body by name, loading it from disk on first access
    /// and caching the result for the lifetime of this registry.
    ///
    /// Returns `None` when no skill with that name is registered, and an
    /// `Err` only if the body genuinely cannot be read.
    pub fn body_for(&self, name: &str) -> Option<Result<String, String>> {
        let skill = self.lock().get(name)?;
        if let Ok(bodies) = self.bodies.read()
            && let Some(cached) = bodies.get(name)
        {
            return Some(Ok(cached.clone()));
        }
        let body = skill.load_body();
        if let Ok(ref text) = body
            && let Ok(mut bodies) = self.bodies.write()
        {
            bodies.insert(name.to_string(), text.clone());
        }
        Some(body)
    }
}

/// Read guard exposing registry contents.
pub struct RegistryGuard<'a> {
    guard: std::sync::RwLockReadGuard<'a, RegistryInner>,
}

impl RegistryGuard<'_> {
    pub fn get(&self, name: &str) -> Option<Skill> {
        self.guard.skills.iter().find(|s| s.name == name).cloned()
    }

    pub fn list(&self) -> Vec<Skill> {
        self.guard.skills.clone()
    }

    pub fn resolve_mentions(&self, text: &str) -> Vec<Skill> {
        render::resolve_mentions(text, &self.guard.skills)
            .into_iter()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shadow(name: &str) -> ShadowedSkill {
        ShadowedSkill {
            name: name.to_string(),
            overridden_scope: SkillScope::User,
            winner_source: std::path::PathBuf::from(format!("/proj/.nuo/skills/{name}/SKILL.md")),
        }
    }

    fn scan_result(shadowed: Vec<ShadowedSkill>) -> DiscoveryResult {
        DiscoveryResult {
            skills: Vec::new(),
            errors: Vec::new(),
            shadowed,
        }
    }

    #[test]
    fn shadow_sink_fires_exactly_once_per_name() {
        let registry = SkillRegistry::empty();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_in_sink = Arc::clone(&seen);
        registry.set_shadow_sink(Some(Arc::new(move |batch: &[ShadowedSkill]| {
            seen_in_sink
                .lock()
                .unwrap()
                .extend(batch.iter().map(|s| s.name.clone()));
        })));

        // First scan reporting a shadow: the sink fires with just that name.
        registry.apply_scan(scan_result(vec![shadow("shared")]));
        assert_eq!(*seen.lock().unwrap(), vec!["shared".to_string()]);

        // A rescan reporting the SAME shadow (the hourly refresh finds the
        // override unchanged) must NOT re-notify — one warning per name.
        registry.apply_scan(scan_result(vec![shadow("shared")]));
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["shared".to_string()],
            "repeat shadow of the same name is deduped"
        );

        // A scan that adds a second shadowed name reports only the new one.
        registry.apply_scan(scan_result(vec![shadow("shared"), shadow("other")]));
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["shared".to_string(), "other".to_string()]
        );

        // The last scan's shadow set remains inspectable regardless of dedupe.
        assert_eq!(registry.shadowed().len(), 2);
    }

    #[test]
    fn shadow_sink_not_installed_means_no_panic_and_records_kept() {
        let registry = SkillRegistry::empty();
        registry.apply_scan(scan_result(vec![shadow("x")]));
        assert_eq!(registry.shadowed().len(), 1);
        // A sink installed LATER does not replay already-reported names.
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_in_sink = Arc::clone(&seen);
        registry.set_shadow_sink(Some(Arc::new(move |batch: &[ShadowedSkill]| {
            seen_in_sink
                .lock()
                .unwrap()
                .extend(batch.iter().map(|s| s.name.clone()));
        })));
        registry.apply_scan(scan_result(vec![shadow("x")]));
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn reactive_watcher_reloads_on_file_change() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let project_root = tmp.path().to_path_buf();
        let skills_dir = project_root.join(".nuo/skills/demo");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(
            skills_dir.join("SKILL.md"),
            "---\nname: demo\ndescription: initial\n---\n# Demo\n",
        )
        .unwrap();

        let config = SkillsConfig {
            project_root: Some(project_root.clone()),
            ..Default::default()
        };

        let registry = SkillRegistry::load(&config).await;
        assert_eq!(
            registry.lock().get("demo").map(|s| s.description.clone()),
            Some("initial".to_string())
        );

        // Spawn reactive watcher
        let _watcher_task = registry.spawn_reactive_watcher();

        // Write updated skill file
        std::fs::write(
            skills_dir.join("SKILL.md"),
            "---\nname: demo\ndescription: updated reactively\n---\n# Demo\n",
        )
        .unwrap();

        // Wait for debounce and reload
        let mut reloaded = false;
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if let Some(skill) = registry.lock().get("demo")
                && skill.description == "updated reactively"
            {
                reloaded = true;
                break;
            }
        }
        assert!(
            reloaded,
            "registry should automatically update via reactive fs watcher"
        );
    }

    #[tokio::test]
    async fn reactive_watcher_populates_empty_registry_on_startup() {
        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path().to_path_buf();
        let skills_dir = project_root.join(".nuo/skills/demo");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(
            skills_dir.join("SKILL.md"),
            "---\nname: demo\ndescription: auto-loaded on startup\n---\n# Demo\n",
        )
        .unwrap();

        let config = SkillsConfig {
            project_root: Some(project_root.clone()),
            ..Default::default()
        };

        // Registry starts empty
        let registry = SkillRegistry::empty_with_config(&config);
        assert!(registry.lock().list().is_empty());

        // Spawn reactive watcher
        let _watcher_task = registry.spawn_reactive_watcher();

        // Must populate automatically without manual reload or /trust
        let mut loaded = false;
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if registry.lock().get("demo").is_some() {
                loaded = true;
                break;
            }
        }
        assert!(
            loaded,
            "empty registry must be populated automatically on watcher startup"
        );
    }
}
