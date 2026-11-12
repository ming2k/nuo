// Integration test target: assertions fail by panicking, so the production
// no-panic lints are meaningless here (the exemption ends at the test boundary).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

// The crate was renamed `nuo-harness` → `nuo-engine`; the suite's historical
// `nuo_harness::` paths stay valid through this alias.
extern crate nuo_engine as nuo_harness;

mod it;
