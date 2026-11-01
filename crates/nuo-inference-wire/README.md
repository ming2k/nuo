# nuo-inference-wire

The model wire substrate: protocol identity, shared decoders, and the wire
vocabulary shipped code actually consumes.

## What this crate is

It is **not** a codec suite, and it is not a second implementation of what the
providers already do. ADR-0084 shrank the old `nuo-model-codec` toward a pure
codec and ADR-0088 pruned its transport; ADR-0143 finished the job by deleting
everything left that had no caller. What remains has a named consumer for every
symbol.

## Live surface

| Symbol | Consumer |
| --- | --- |
| `WireProtocol` | every provider spec, route derivation, capability registry |
| `find_balanced_object` | harness tool-call router; two provider stream parsers |
| `loop_detector::{StreamLoopDetector, DegeneratePattern}` | harness stream governance |
| `token_classification::{TokenBreakdown, TokenClass}` | provider pricing |
| `protocol::google_interactions::{parse_response, parse_chunk}` | `nuo-provider-google-antigravity` |
| `types::*` | the payload structs the above return |
| `json`, `wire_protocol` | module paths the above are reached through |

`parse_response` and `parse_chunk` are **total**: they never fail, degrading to
empty fields instead. There is no error type in this crate.

## What was removed and why

Deleted with no shim, alias, or deprecation period (ADR-0143):

- `discriminative/` — the System One lane. ADR-0086 recorded it as having
  **zero production consumers** and relocated it instead of removing it; one year
  later it still had none.
- `credentials.rs`, `endpoint.rs` — `StaticApiKey`, `DynamicCredentials`,
  `CredentialsProvider`, `ProviderVendor`, the codec-side `Endpoint` (a
  near-duplicate of the live `nuo_provider::endpoint::Endpoint`), `WireClient`.
- The five per-vendor `build_request` implementations and the
  `protocol::build_request` / `parse_response` dispatcher they fed. No shipped
  binary called the dispatcher; every provider builds and parses its own body.
- `stream.rs` — `SseDecoder`, `parse_vendor_chunk`, `StreamAccumulator`. The SSE
  decoder actually used is `nuo_provider::transport::sse`.
- `error.rs` — `WireError` had zero constructors and zero external consumers.

## Guarantees

The dependency set is `serde`, `serde_json`, `thiserror`, `uuid`. This crate
therefore cannot perform network I/O, cannot hold a transport, and cannot host an
async runtime — `[INV-AP-01]` is a manifest fact, not a paragraph. `[INV-AP-03]`
forbids re-growing an unwired second implementation: a codec may live here only
if a crate *outside* it calls that codec from `src`, not merely from tests.
