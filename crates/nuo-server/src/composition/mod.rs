//! Composition root: the only modules allowed to construct providers, tools,
//! stores, and the per-session driver.
//!
//! Product policy that shapes the composition (project scaffolding, built-in
//! slash vocabulary) lives here or in `product/`; the session runtime in
//! `runtime/` must not depend on it.

pub mod agent_setup;
pub mod bootstrap;
pub mod credentials_host;
pub mod kernel_host;
#[cfg(test)]
mod provider_baseline_fidelity_tests;
pub mod provider_baselines;
pub mod provider_registry;
pub mod skills_host;
pub mod startup;
