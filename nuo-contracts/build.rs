//! Build script: pins ts-rs's large-integer mapping for the web wire-type
//! export (`apps/web/src/lib/generated/wire.gen.ts`).
//!
//! ts-rs renders `u64`/`i64` as TypeScript `bigint` by default, but the wire
//! protocol is JSON, where every integer is a plain number. The generated
//! `#[ts(export)]` tests build their config via `ts_rs::Config::from_env()`,
//! which reads `TS_RS_LARGE_INT` at test runtime; cargo propagates
//! `rustc-env` variables to the package's own test binaries, so
//! `cargo test -p muta-contracts` regenerates the bindings with `number`
//! and no caller-side setup.
//!
//! # Regenerate with the FULL test run, never a filtered one
//!
//! ts-rs 12 writes the export file per *test binary run*: the first export
//! executed in a process overwrites the file and subsequent ones merge into
//! it. A filtered run (`cargo test -p muta-contracts wire::`) executes
//! only a subset of the export tests, so its first write truncates the file
//! to just those types. Always regenerate with the unfiltered
//! `cargo test -p muta-contracts` (CI does exactly this and fails on any
//! drift, so a truncated file cannot merge).
fn main() {
    println!("cargo::rustc-env=TS_RS_LARGE_INT=number");
}
