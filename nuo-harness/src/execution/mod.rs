//! Concrete execution environments and pipeline middleware.

pub mod in_memory;
pub mod local;
pub mod middleware;

pub use in_memory::{InMemoryExecutionEnvironment, InMemoryFsProvider, MockProcessRunner};
pub use local::{
    LocalExecutionEnvironment, LocalFsProvider, LocalProcessRunner, WorkspaceExecutionEnvironment,
    workspace_sandbox_available,
};
pub use middleware::{SecretScrubMiddleware, WorkspaceJailMiddleware};

#[cfg(test)]
pub(crate) use local::workspace_tests::workspace_tests_outside_scratch;

#[cfg(test)]
mod tests;
