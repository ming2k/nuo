//! Schema catalog and migration machinery (0186/0187/0236).
//!
//! Split out of `db.rs`; still inside `nuo-persistence::db`, so
//! one-door boundary is unchanged.

pub mod catalog;
pub mod runner;
pub mod types;
pub mod v01_v15;
pub mod v16_v25;
pub mod v26_v30;

pub use catalog::MIGRATIONS;
pub use runner::*;
pub use types::*;
pub use v01_v15::*;
pub use v16_v25::*;
pub use v26_v30::*;
