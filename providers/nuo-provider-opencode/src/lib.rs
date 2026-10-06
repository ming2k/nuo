//! OpenCode (Console, Plan, Zen) provider channel for Nuo (ADR-0015).

#[cfg(test)]
extern crate nuo_provider_deepseek;

pub mod console;
pub mod device;
pub mod plan;
pub mod zen;

pub use console::MODEL_PROVIDER_SPEC as CONSOLE_SPEC;
pub use device::*;
pub use plan::MODEL_PROVIDER_SPEC as PLAN_SPEC;
pub use zen::MODEL_PROVIDER_SPEC as ZEN_SPEC;

pub mod oauth;
