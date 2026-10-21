//! System One decision engine: typed, machine-native probabilistic decision
//! support for models such as TypeSafe Jev.
//!
//! Split from the wire crate per ADR-0084 S2: decision evaluation is a
//! distinct business face that shares transport (`WireClient`, wire error
//! vocabulary, credential sources) with the generative lane but no semantics
//! with it. This crate owns the decision lifecycle — questions, answers,
//! calibrated probability responses, and driver clients — while the wire
//! crate owns the shared transport substrate.

pub mod decision;

pub use decision::{
    ChoiceAnswer, ChoiceQuestion, DecisionAnswer, DecisionClient, DecisionEndpoint,
    DecisionProtocol, DecisionQuestion, DecisionRequest, DecisionResponse, DecisionUsage,
    NoulAnswer, NoulQuestion, ScoreAnswer, ScoreQuestion,
};

// Shared transport vocabulary (re-exported for consumer convenience; the
// authority stays in the wire crate).
pub use nuo_inference_wire::{CredentialsProvider, Result, StaticApiKey, WireError, error};
