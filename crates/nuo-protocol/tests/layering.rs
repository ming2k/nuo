//! Layer-0 dependency invariant for `nuo-protocol`.
//!
//! The protocol crate is the bottom of the graph: wire contracts and pure
//! domain vocabulary only. Anything with OS I/O, network transports, or an
//! agent runtime must depend *down* on it, never the other way around.

#[test]
fn protocol_manifest_stays_at_layer_zero() {
    let manifest = include_str!("../Cargo.toml");
    for forbidden in [
        "nuo-host",
        "nuo-provider",
        "nuo-mcp",
        "nuo-server",
        "nuo-engine",
        "nuo-persistence",
        "netune",
        "dirs",
        "arboard",
        "notify",
    ] {
        assert!(
            !manifest.contains(forbidden),
            "nuo-protocol must not depend on `{forbidden}`; that edge points upward and \
             forces every Layer-0 consumer to compile OS/network/runtime stacks"
        );
    }
}
