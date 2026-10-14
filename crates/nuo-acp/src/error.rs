use thiserror::Error;

#[derive(Error, Debug, Clone)]
pub enum ProtocolError {
    #[error("invalid agent address: {0}")]
    InvalidAddress(String),

    #[error("recipient unreachable: {0}")]
    Unreachable(String),

    #[error("routing error: {0}")]
    RoutingError(String),

    #[error("transport error: {0}")]
    Transport(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("delegation error: {0}")]
    DelegationError(String),

    #[error("delegation hop limit exceeded (max {0} hops); refusing to forward further")]
    HopLimitExceeded(u32),

    #[error("request timed out after {0} ms")]
    RequestTimeout(u64),

    #[error("room error: {0}")]
    RoomError(String),

    #[error("invalid channel id: {0}")]
    InvalidChannelId(String),

    #[error("channel not found: {0}")]
    ChannelNotFound(String),

    #[error("not subscribed: {0}")]
    NotSubscribed(String),

    #[error("routing policy denied delivery: {0}")]
    PolicyDenied(String),
}

pub type Result<T> = std::result::Result<T, ProtocolError>;
