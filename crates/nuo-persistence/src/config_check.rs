//! Domain-matrix configuration validation (ADR-0031).
//!
//! The active schema is **strict**: every domain file is parsed with
//! `deny_unknown_fields`, so an unknown key, a retired spelling, or a type
//! error is a hard load failure — never a silent fall-back to defaults.
//! `nuo config check` re-runs that strict parse over each domain file and
//! reports the failure per file, so a typo can be found before the next `nuo`
//! invocation refuses to start.

use std::fs;
use std::path::PathBuf;

use crate::config::Config;

/// One finding from a validation pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigFinding {
    /// The file the finding belongs to.
    pub key: String,
    pub message: String,
    /// Retained for API compatibility; the strict schema reports no
    /// "legacy-but-tolerated" keys.
    pub is_legacy: bool,
}

/// Validate the ADR-0031 domain matrix. The `path` argument is accepted for
/// API compatibility and ignored: the resolved domain-file paths are checked.
/// An empty result means every present domain file parses strictly.
pub fn check_config_file(_path: Option<PathBuf>) -> Vec<ConfigFinding> {
    let mut findings = Vec::new();
    for (label, path) in [
        ("server.toml", Config::server_config_file_path()),
        ("client.toml", Config::client_config_file_path()),
        ("agent.toml", Config::agent_config_file_path()),
    ] {
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        if let Err(message) = Config::validate_domain_source(label, &content) {
            findings.push(ConfigFinding {
                key: label.to_string(),
                message,
                is_legacy: false,
            });
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_domain_files_produce_no_findings() {
        let (_tmp, _guard, _override_guard) = crate::config::tests::sandbox_config_dir();
        // No domain files written yet: nothing to report.
        assert!(check_config_file(None).is_empty());
    }

    #[test]
    fn unknown_key_in_a_domain_file_is_a_finding() {
        let (_tmp, _guard, _override_guard) = crate::config::tests::sandbox_config_dir();
        std::fs::write(
            Config::client_config_file_path(),
            "default_conection = \"typo\"\n",
        )
        .unwrap();
        let findings = check_config_file(None);
        assert_eq!(findings.len(), 1, "got: {findings:?}");
        assert_eq!(findings[0].key, "client.toml");
        assert!(findings[0].message.contains("unknown field"), "{findings:?}");
    }

    #[test]
    fn type_error_in_a_domain_file_is_a_finding() {
        let (_tmp, _guard, _override_guard) = crate::config::tests::sandbox_config_dir();
        std::fs::write(
            Config::server_config_file_path(),
            "[lifecycle]\nshutdown_grace_secs = \"soon\"\n",
        )
        .unwrap();
        let findings = check_config_file(None);
        assert_eq!(findings.len(), 1, "got: {findings:?}");
        assert_eq!(findings[0].key, "server.toml");
    }
}
