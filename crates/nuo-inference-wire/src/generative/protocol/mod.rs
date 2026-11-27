//! Vendor protocol modules that still carry live decoding.
//!
//! There is deliberately **no dispatcher here**. `build_request` /
//! `parse_response` used to fan out over five vendors from this module, and
//! nothing outside its own tests ever called the fan-out: every shipped
//! provider builds and parses its own wire body. A dispatcher with zero
//! production callers is not an abstraction, it is a second implementation
//! silently drifting away from the real one.
//!
//! What remains is the single vendor whose parser is genuinely shared
//! (`google_interactions`, consumed by `nuo-provider-google-antigravity`).

pub mod google_interactions;
