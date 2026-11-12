//! Durable, event-driven context added to an agent's live conversation window.
//!
//! These messages are distinct from request projection: lifecycle owners decide
//! when to append them, and the resulting provenance survives persistence.
//!
//! `@`-references are *asset references*: they are resolved here, on
//! the durable side, into canonical envelopes; `mentions` owns the shared
//! lexical helpers (code-span masking, boundary guards, address
//! canonicalization) used by the resolvers and by the request projection.
mod files;
mod mentions;
mod skills;

pub(crate) use files::inject_mentioned_files;
pub(crate) use mentions::{canonicalize_addresses, is_reference_site};
pub(crate) use skills::inject_mentioned_skills;
// Message constructors are a leaf (`crate::runtime::message`) so the runtime,
// dispatch, and orchestration layers can build context messages without
// depending on cognition (`[INV-HARNESS-04]`, ADR-0101). Only the constructor
// the conversation-context submodules share is re-exported here.
pub(crate) use crate::runtime::message::hidden_user_with_reason;
