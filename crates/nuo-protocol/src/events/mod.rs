//! Wire types for the harness ↔ driver protocol: requests ([`AgentRequest`]),
//! responses ([`AgentResponse`]), live agent events ([`AgentEvent`]), and the
//! small data records they carry.

pub(crate) use crate::{Availability, TrustDomain};

pub(crate) use nuo_tool::{ImagePart, Message, ToolOutput, ToolStream};

pub(crate) use serde::{Deserialize, Serialize};

// Relocated to the tool leaf (`nuo_tool::events`) per ADR-0008 §3.
pub use nuo_tool::events::{
    AgentNotice, NoticeKind, NoticeSeverity, NoticeSource, PermissionRequest, StdinRequest,
    SubagentEvent, UserQuestion, UserQuestionOption, UserQuestionRequest,
};

mod permission;
mod provider;
mod request;
mod response;
mod round;
mod session;
mod status;
mod thread;

pub use self::permission::*;
pub use self::provider::*;
pub use self::request::*;
pub use self::response::*;
pub use self::round::*;
pub use self::session::*;
pub use self::status::*;
pub use self::thread::*;

#[cfg(test)]
mod tests;
