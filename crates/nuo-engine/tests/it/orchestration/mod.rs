#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Orchestration-layer integration tests: provider retry behavior, the
//! proxy provider, retry-delay math, context-overflow classification, and
//! the self-registration of built-in tools via `inventory`. These live with
//! the code under test (they were historically parked in the `nuo`
//! binary, which exercised this layer end-to-end before moved
//! session hosting into the server).
//!
//! The suite is split by scenario; shared imports and fixtures are re-exported
//! here so every child states only its own domain logic.

pub(super) use futures::StreamExt;
pub(super) use futures::stream;
pub(super) use nuo_harness::orchestration::{
    ContextProjectionSettings, InteractiveRoundContext, ProxyProvider, RoundContext, RoundInput,
    execute_round, start_interactive_round,
};
pub(super) use nuo_harness::runtime::retry::{apply_jitter_ms, retry_delay_ms};
pub(super) use nuo_harness::{Agent, RoundLifecycle, SkillRegistry};
pub(super) use nuo_persistence::session::SessionStore;
pub(super) use nuo_protocol::{
    AgentResponse, Message, Provider, ProviderStreamEvent, Role, RoundEvent, ToolContextBuilder,
    async_trait, collect_toolset,
};
pub(super) use std::sync::atomic::{AtomicUsize, Ordering};
pub(super) use std::sync::{Arc, Mutex, RwLock};
pub(super) use tokio::sync::mpsc;
pub(super) use tokio_util::sync::CancellationToken;

mod common;
mod images;
mod interrupts;
mod projection;
mod registry;
mod retry;
mod retry_budget;
mod stream_tools;
mod truncation;

pub(super) use retry::VisibleFailProvider;
