//! Re-export of the protocol-owned secret string type.
//!
//! The implementation lives in `nuo-protocol` so discovery/config contracts can
//! carry credentials without depending on the host runtime.

pub use nuo_protocol::secret::*;
