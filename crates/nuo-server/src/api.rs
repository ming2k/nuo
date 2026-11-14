//! Small facade consumed by the `nuo` binary.
//!
//! The binary should not reach into every internal module. These re-exports are
//! the stable application-core surface; everything else is reachable through
//! the group modules for tests and in-crate composition.

pub use crate::composition::bootstrap;
pub use crate::composition::provider_registry;
pub use crate::composition::startup;
pub use crate::runtime::host;
pub use crate::runtime::input_completion;
pub use crate::runtime::registry::{HostedThread, ThreadRegistry};
pub use crate::runtime::thread_driver::ThreadDriver;
pub use crate::transport::serve_discovery;
pub use crate::transport::ui_bridge::{CopyOutcome, UiBridge};
