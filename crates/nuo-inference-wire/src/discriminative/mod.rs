//! System One discriminative primitives, typed questions/answers, and wire drivers.
//!
//! Provides machine-native, calibrated probabilistic decision and scoring support for
//! non-generative models such as TypeSafe Jev.

pub mod endpoint;
pub mod protocol;
pub mod types;

pub use endpoint::DiscriminativeEndpoint;
pub use protocol::DiscriminativeProtocol;
pub use types::{
    Answer, ChoiceAnswer, ChoiceQuestion, DiscriminativeRequest, DiscriminativeResponse,
    DiscriminativeUsage, NoulAnswer, NoulQuestion, Question, ScoreAnswer, ScoreQuestion,
};
