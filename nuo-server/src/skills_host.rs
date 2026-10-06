//! The application plane's skills host: where the product keeps skills, and who
//! may admit a workspace's own skill content.
//!
//! `nuo-skills` is a kernel crate: it discovers and serves skills, but it does
//! not know where the product stores anything and it does not own the trust
//! decision (ADR-0300 §1, ADR-0303 §1). Both enter through
//! [`nuo_harness::skills::SkillHost`], and this module is the shipped product's
//! implementation of that seam.
//!
//! Keeping the resolution here — rather than inside the kernel — is what lets a
//! different embedding run skills out of a completely different directory tree,
//! or out of none at all (`SkillHost::none`).

use std::path::Path;
use std::sync::Arc;

use nuo_wire::WorkspaceTrustState;
use nuo_persistence::paths;
use nuo_persistence::workspace_security::WorkspaceSecurityStore;
use nuo_harness::skills::{SkillHost, SkillRoots, SkillTrust};

/// The product's skill roots, resolved from the path topology (ADR-0013) so
/// `--cache-dir` / `$XDG_DATA_HOME` / `NUO_DATA_DIR` overrides all land in one
/// place.
pub fn roots(role: Option<&str>) -> SkillRoots {
    let dirs = paths::get();
    SkillRoots {
        user: dirs.user_skills_dir(),
        role: role.map(|role| dirs.role_skills_dir(role)),
        remote_cache: dirs.remote_skills_cache(),
    }
}

/// A complete host for the shipped product: topology-resolved roots and the
/// durable workspace-security store as the admission authority.
pub fn host(role: Option<&str>) -> SkillHost {
    SkillHost::new(roots(role), Arc::new(DurableWorkspaceTrust))
}

/// The skills-domain admission authority, read live from the durable store on
/// every question.
///
/// Reading per call is deliberate: `/trust skills` and `/skills reload` must
/// take effect immediately, and a cached verdict would let a revoked grant keep
/// serving project-authored content until the next process start.
pub struct DurableWorkspaceTrust;

impl SkillTrust for DurableWorkspaceTrust {
    fn skills_state(&self, workspace: &Path) -> WorkspaceTrustState {
        WorkspaceSecurityStore::load().snapshot(workspace).skills
    }
}
