use thiserror::Error;

#[derive(Error, Debug, Clone)]
pub enum AgentError {
    #[error("provider error: {0}")]
    Provider(String),

    #[error("tool execution error on `{0}`: {1}")]
    Tool(String, String),

    #[error("token budget exceeded: {0}")]
    BudgetExceeded(String),

    #[error("maximum round steps exceeded ({0} rounds)")]
    MaxRoundsExceeded(u32),

    #[error("delegation error: {0}")]
    Delegation(String),

    #[error("session error: {0}")]
    Session(String),

    #[error("session busy: {0}")]
    SessionBusy(String),

    #[error("compaction error: {0}")]
    Compaction(String),

    #[error("execution cancelled: {0}")]
    Cancelled(String),

    #[error("protocol error: {0}")]
    Protocol(#[from] acp::ProtocolError),
}

pub type Result<T> = std::result::Result<T, AgentError>;
