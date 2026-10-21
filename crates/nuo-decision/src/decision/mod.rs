//! System One decision primitives, typed questions/answers, and wire drivers.
//!
//! Provides machine-native, calibrated probabilistic decision support for models such as
//! TypeSafe Jev, decoupled from generative chat completions loops.

pub mod client;
pub mod endpoint;
pub mod protocol;
pub mod types;

pub use client::DecisionClient;
pub use endpoint::DecisionEndpoint;
pub use protocol::DecisionProtocol;
pub use types::{
    ChoiceAnswer, ChoiceQuestion, DecisionAnswer, DecisionQuestion, DecisionRequest,
    DecisionResponse, DecisionUsage, NoulAnswer, NoulQuestion, ScoreAnswer, ScoreQuestion,
};
