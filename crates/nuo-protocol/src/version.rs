//! Protocol and product version negotiation, compatibility checks, and mismatch diagnostics.

use crate::discovery::Discovery;
use crate::{MIN_PROTOCOL_VERSION, PROTOCOL_VERSION};
use std::path::{Path, PathBuf};

/// Stable machine-readable error codes.
pub const ERR_PROTOCOL_MISMATCH: &str = "protocol_mismatch";
pub const ERR_VERSION_MISMATCH: &str = "version_mismatch";

/// Check whether the server's declared wire protocol number is within this client's supported window.
pub const fn protocol_accepts(client: u32) -> bool {
    matches!(client, MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION)
}

/// Directional relation between client version and server version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionRelation {
    Equal,
    ClientNewer,
    ClientOlder,
    Unknown,
}

/// Compare client and server version strings using SemVer.
pub fn compare_versions(client: &str, server: &str) -> VersionRelation {
    if client == server {
        return VersionRelation::Equal;
    }
    match (
        semver::Version::parse(client),
        semver::Version::parse(server),
    ) {
        (Ok(c), Ok(d)) => {
            if c > d {
                VersionRelation::ClientNewer
            } else if c < d {
                VersionRelation::ClientOlder
            } else {
                VersionRelation::Equal
            }
        }
        _ => VersionRelation::Unknown,
    }
}

/// The actionable error for a discovered-but-incompatible server,
/// preferring the wire-protocol explanation when the record declares a
/// protocol number outside this client's window, then the
/// dev-drift explanation (same version, replaced binary), and falling
/// back to the product-version message for legacy records.
pub fn incompatibility_error(info: &Discovery) -> String {
    if info.protocol.is_some_and(|p| !protocol_accepts(p)) {
        protocol_mismatch(info)
    } else if info.version.as_deref() == Some(env!("CARGO_PKG_VERSION"))
        && !server_image_is_current(info)
    {
        format!(
            "client/server binary mismatch: running server (pid {}, version {}) executable differs from the installed nuo core image (rebuilt binary). \
             Stop it with `nuo stop` and rerun — the server restarts on demand.",
            info.pid,
            env!("CARGO_PKG_VERSION")
        )
    } else {
        version_mismatch(info)
    }
}

/// The actionable version-skew error, naming both builds
/// and the directional fix (server behind -> stop/restart server; client behind -> update client).
pub fn version_mismatch(info: &Discovery) -> String {
    let client_ver = env!("CARGO_PKG_VERSION");
    let Some(server_ver) = info.version.as_deref() else {
        return format!(
            "client/server version mismatch: this client is {client_ver} but the running server (pid {}) is unknown (older than 0.24). \
             Stop it with `nuo stop` and rerun — the server restarts on demand at the new version.",
            info.pid
        );
    };

    match compare_versions(client_ver, server_ver) {
        VersionRelation::ClientNewer => format!(
            "client/server version mismatch: running server (pid {}, version {server_ver}) is older than this client ({client_ver}). \
             Stop it with `nuo stop` and rerun — the server restarts on demand at the new version.",
            info.pid
        ),
        VersionRelation::ClientOlder => format!(
            "client/server version mismatch: this client ({client_ver}) is older than the running server (pid {}, version {server_ver}). \
             Please update your nuo client to {server_ver} or newer.",
            info.pid
        ),
        VersionRelation::Equal => {
            if !server_image_is_current(info) {
                format!(
                    "client/server binary mismatch: running server (pid {}, version {server_ver}) executable differs from the installed nuo core image (rebuilt binary). \
                     Stop it with `nuo stop` and rerun — the server restarts on demand.",
                    info.pid
                )
            } else {
                format!(
                    "client/server version mismatch: client {client_ver} vs server {server_ver}. \
                     Stop the server and let it restart on demand: `nuo stop`, then rerun \
                     this command (or `nuo start` to bring it up explicitly)."
                )
            }
        }
        VersionRelation::Unknown => format!(
            "client/server version mismatch: client {client_ver} vs server {server_ver}. \
             Stop the server and let it restart on demand: `nuo stop`, then rerun \
             this command (or `nuo start` to bring it up explicitly)."
        ),
    }
}

/// The actionable protocol-mismatch error.
pub fn protocol_mismatch(info: &Discovery) -> String {
    let client_proto = PROTOCOL_VERSION;
    let min_proto = MIN_PROTOCOL_VERSION;
    let Some(server_proto) = info.protocol else {
        return format!(
            "protocol mismatch: this client speaks protocol {client_proto} but the running server (pid {}) declares no protocol. \
             Stop it with `nuo stop` and rerun — the server restarts on demand.",
            info.pid
        );
    };

    if server_proto < min_proto {
        format!(
            "protocol mismatch: running server (pid {}, protocol {server_proto}) is older than the minimum supported by this client ({min_proto}..={client_proto}). \
             Stop it with `nuo stop` and rerun — the server restarts on demand.",
            info.pid
        )
    } else {
        format!(
            "protocol mismatch: this client (protocol {client_proto}) is older than the running server (pid {}, protocol {server_proto}). \
             Please update your nuo client to match.",
            info.pid
        )
    }
}

/// Check if running server is compatible with this client.
pub fn versions_compatible(info: &Discovery) -> bool {
    local_pair_compatible(
        info.protocol,
        info.version.as_deref(),
        server_image_is_current(info),
    )
}

/// Pure compatibility decision helper.
pub fn local_pair_compatible(
    protocol: Option<u32>,
    version: Option<&str>,
    server_image_is_current: bool,
) -> bool {
    if let Some(proto) = protocol {
        if !protocol_accepts(proto) {
            return false;
        }
        let same_version = version == Some(env!("CARGO_PKG_VERSION"));
        return !same_version || server_image_is_current;
    }
    version.is_some_and(|v| v == env!("CARGO_PKG_VERSION")) && server_image_is_current
}

/// Whether the running server's executable is still the resolved `nuo` core image.
pub fn server_image_is_current(info: &Discovery) -> bool {
    let expected = server_program();
    if !expected.is_file() {
        return true;
    }
    if info.image_digest.is_some() {
        return content_matches_record(image_content_digest(&expected), info);
    }
    server_image_is_current_by_inode(info.pid)
}

fn content_matches_record(installed: Option<(u64, String)>, info: &Discovery) -> bool {
    let Some(published) = info.image_digest.as_deref() else {
        return true;
    };
    match installed {
        Some((len, digest)) => {
            let len_matches = info
                .image_len
                .is_none_or(|expected_len| expected_len == len);
            len_matches && digest.eq_ignore_ascii_case(published)
        }
        None => true,
    }
}

fn server_image_is_current_by_inode(pid: u32) -> bool {
    let expected = server_program();
    if !expected.is_file() {
        return true;
    }
    nuo_host::process::process_image_matches_path(pid, &expected)
}

fn image_content_digest(path: &Path) -> Option<(u64, String)> {
    nuo_host::process::image_digest_len(path)
}

fn server_program() -> PathBuf {
    if let Some(program) = std::env::var_os("NUO_BIN").filter(|value| !value.is_empty()) {
        return PathBuf::from(program);
    }
    if let Ok(current) = std::env::current_exe() {
        let sibling_nuo = current.with_file_name(format!("nuo{}", std::env::consts::EXE_SUFFIX));
        if sibling_nuo.is_file() {
            return sibling_nuo;
        }
    }
    PathBuf::from(format!("nuo{}", std::env::consts::EXE_SUFFIX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compare_versions() {
        assert_eq!(compare_versions("0.25.0", "0.25.0"), VersionRelation::Equal);
        assert_eq!(
            compare_versions("0.26.0", "0.25.0"),
            VersionRelation::ClientNewer
        );
        assert_eq!(
            compare_versions("0.24.0", "0.25.0"),
            VersionRelation::ClientOlder
        );
        assert_eq!(
            compare_versions("1.0.0", "0.25.0"),
            VersionRelation::ClientNewer
        );
        assert_eq!(
            compare_versions("not-a-semver", "0.25.0"),
            VersionRelation::Unknown
        );
    }

    #[test]
    fn test_local_pair_compatible() {
        let me = env!("CARGO_PKG_VERSION");
        assert!(local_pair_compatible(
            Some(PROTOCOL_VERSION),
            Some("0.0.1-much-older"),
            false,
        ));
        assert!(local_pair_compatible(
            Some(MIN_PROTOCOL_VERSION),
            Some("99.0.0"),
            false,
        ));
        assert!(!local_pair_compatible(
            Some(PROTOCOL_VERSION + 1),
            Some(me),
            true,
        ));
        assert!(!local_pair_compatible(Some(0), Some("0.0.1"), true));
        assert!(local_pair_compatible(
            Some(PROTOCOL_VERSION),
            Some(me),
            true,
        ));
        assert!(local_pair_compatible(None, Some(me), true));
        assert!(!local_pair_compatible(None, Some("0.0.0"), true));
        assert!(!local_pair_compatible(None, None, true));
    }

    #[test]
    fn test_version_mismatch_messages() {
        let server_older = Discovery {
            pid: 1234,
            process_birth_token: None,
            port: 9800,
            token: None,
            project_root: String::new(),
            started_at: 0,
            uds_path: None,
            local_endpoint: None,
            version: Some("0.0.0".to_string()),
            grace_secs: None,
            protocol: None,
            image_digest: None,
            image_len: None,
        };
        let msg = version_mismatch(&server_older);
        assert!(msg.contains("is older than this client"));
        assert!(msg.contains("nuo stop"));

        let server_newer = Discovery {
            pid: 1234,
            process_birth_token: None,
            port: 9800,
            token: None,
            project_root: String::new(),
            started_at: 0,
            uds_path: None,
            local_endpoint: None,
            version: Some("99.0.0".to_string()),
            grace_secs: None,
            protocol: None,
            image_digest: None,
            image_len: None,
        };
        let msg = version_mismatch(&server_newer);
        assert!(msg.contains("older than the running server"));
        assert!(msg.contains("update your nuo client"));

        let server_none = Discovery {
            pid: 1234,
            process_birth_token: None,
            port: 9800,
            token: None,
            project_root: String::new(),
            started_at: 0,
            uds_path: None,
            local_endpoint: None,
            version: None,
            grace_secs: None,
            protocol: None,
            image_digest: None,
            image_len: None,
        };
        let msg = version_mismatch(&server_none);
        assert!(msg.contains("unknown (older than 0.24)"));
        assert!(msg.contains("nuo stop"));
    }
}
