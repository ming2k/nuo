//! Native operating-system capabilities used by nuo's business layers.
//!
//! The public API is expressed in semantic operations (local IPC, an owned
//! process tree, server detachment, and an advisory process lock). OS-specific
//! mechanisms stay behind those boundaries so callers never emulate a missing
//! capability with a successful no-op.

// Unit tests assert by panicking: a failed `unwrap`/`expect`/`panic!` *is* the
// test result. `cfg_attr(test, ..)` scopes that exemption to the test build only,
// so the shipped graph stays under the workspace `deny` for these lints.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod clipboard;
pub mod color_scheme;
pub mod environment;
pub mod fs;
pub mod fs_watcher;
pub mod fsutil;
pub mod ipc;
pub mod lock;
pub mod opener;
pub mod paths;
pub mod process;
pub mod secret;
pub mod secure_file;
pub mod security;
pub mod shared_roots;
pub mod shell;
pub mod supervised;
pub mod web_config;
pub mod workspace;
pub mod workspace_sandbox;

pub use color_scheme::*;
pub use environment::{detect_device_fingerprint, detect_runtime_environment, generate_session_id};
pub use fs_watcher::{FsEvent, FsEventKind, FsWatcher};
pub use secret::SecretString;
pub use security::{
    AssetLocator, AssetSpec, AttestationStatus, PROJECT_RULE_FILES, TrustDomain,
    WorkspaceSecuritySnapshot, WorkspaceTrustState,
};
pub use shared_roots::SharedAdditionalRoots;
pub use supervised::{
    BackgroundJobInfo, BackgroundJobOutcome, BackgroundJobService, JobId, JobKind, JobSpec,
    JobState, Readiness, RestartPolicy,
};
pub use web_config::*;
pub use workspace::{WorkspaceBinding, WorkspaceFilter};

#[cfg(windows)]
mod windows_security;

#[cfg(not(any(unix, windows)))]
compile_error!("nuo-platform supports Unix and Windows targets only");
