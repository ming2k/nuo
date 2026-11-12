//! Token-source accounting: how many tokens came from authoritative upstream
//! usage reports vs. local estimates.
//!
//! When a provider reports real `usage` ([`crate::ProviderCompletionMeta::usage`] or
//! a [`crate::ProviderStreamEvent::Usage`]), the harness books those tokens as
//! **reported**. When it does not, the harness falls back to the local
//! char-class estimator ([`crate::estimate_tokens`]) and books them as
//! **estimated**. This module keeps a running tally so the UI can answer
//! "how accurate is my context meter?" and surface which providers/models
//! are measured vs. guessed.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

mod ledger;
mod record;
mod report;
mod request;

#[cfg(test)]
mod tests;

pub use self::ledger::*;
pub use self::record::*;
pub use self::report::*;
pub use self::request::*;
