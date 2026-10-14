//! Host-supplied skill roots and workspace trust.
//!
//! Skills are a capability the kernel *uses*, not state it *owns*. Two facts
//! belong to the application plane and enter here as inputs:
//!
//! - **Where skills live.** The product resolves its directories (XDG user
//!   skills, role-scoped skills, the remote cache) and hands them in as
//!   [`SkillRoots`]. The kernel has no durable state to locate (ADR-0300 §1),
//!   so this crate never consults a path resolver.
//! - **Whether a workspace's skill content is admitted.** Repo-scoped skills are
//!   a prompt-injection surface, so the *host* owns the trust decision and
//!   exposes it through [`SkillTrust`]. The kernel asks per use and
//!   never caches a verdict.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use nuo_wire::WorkspaceTrustState;

/// Host-resolved skill directories.
///
/// Every field is a decision the product made; the kernel only reads them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SkillRoots {
    /// User-global skill root (e.g. `<XDG_DATA_HOME>/nuo/skills`).
    pub user: PathBuf,
    /// Role-scoped skill root, when the session runs under a role with one
    /// (ADR-0253).
    pub role: Option<PathBuf>,
    /// Cache root for remote skill repositories.
    pub remote_cache: PathBuf,
}

impl SkillRoots {
    /// Roots for a host that has nothing to offer: no directories, so nothing is
    /// discoverable. Used by an embedding that installs skill tools but supplies
    /// no skill sources.
    pub fn none() -> Self {
        Self::default()
    }
}

/// The workspace trust decision for repo-scoped skill content.
///
/// Implemented by the application plane over whatever authority it keeps (the
/// shipped product reads its durable workspace-security store). The kernel asks
/// once per admission check and holds no cache — a revoked trust must take
/// effect on the next use, not on the next restart.
pub trait SkillTrust: Send + Sync {
    /// Admission state of `workspace`'s skills domain.
    fn skills_state(&self, workspace: &Path) -> WorkspaceTrustState;
}

/// A host that admits nothing.
///
/// With [`SkillRoots::none`] there are no repo-scoped skills to admit, so this
/// pair describes an embedding with no skill sources at all: nothing is
/// discovered and nothing would be admitted if it were. There is no implicit
/// "trusted" fallback anywhere in this crate.
#[derive(Debug, Default, Clone, Copy)]
pub struct DenyAllTrust;

impl SkillTrust for DenyAllTrust {
    fn skills_state(&self, _workspace: &Path) -> WorkspaceTrustState {
        WorkspaceTrustState::Denied
    }
}

/// The kernel-facing host handle: where skills come from, and who may admit
/// them.
#[derive(Clone)]
pub struct SkillHost {
    roots: SkillRoots,
    trust: Arc<dyn SkillTrust>,
}

impl SkillHost {
    pub fn new(roots: SkillRoots, trust: Arc<dyn SkillTrust>) -> Self {
        Self { roots, trust }
    }

    /// A host with no sources and no admissions. See [`DenyAllTrust`].
    pub fn none() -> Self {
        Self {
            roots: SkillRoots::none(),
            trust: Arc::new(DenyAllTrust),
        }
    }

    pub fn roots(&self) -> &SkillRoots {
        &self.roots
    }

    pub fn trust(&self) -> &dyn SkillTrust {
        self.trust.as_ref()
    }
}

impl std::fmt::Debug for SkillHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillHost")
            .field("roots", &self.roots)
            .finish_non_exhaustive()
    }
}
