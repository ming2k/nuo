//! Universal Asset Attestation Ledger (ADR-0243, ADR-0252): Cryptographic
//! fingerprinting, composite identity, replacement semantics, and bounded 30-day leases.

use crate::db::PersistenceHandle;
use nuo_wire::security::{AssetLocator, AssetSpec, AttestationStatus};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Default lease duration: 30 calendar days in seconds (ADR-0252).
pub const ATTESTATION_LEASE_TTL_SECS: u64 = 30 * 86_400;

/// Persisted record of an asset attestation grant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetAttestationRecord {
    pub locator: AssetLocator,
    pub fingerprint: String,
    pub spec: AssetSpec,
    pub status: AttestationStatus,
    pub trusted_at_s: u64,
    pub expires_at_s: u64,
}

/// Durable store for universal asset attestation decisions (ADR-0243, ADR-0252).
#[derive(Debug, Clone)]
pub struct AssetAttestationLedger {
    handle: PersistenceHandle,
}

impl AssetAttestationLedger {
    pub fn load() -> Self {
        Self {
            handle: crate::db::get_persistence_handle(),
        }
    }

    pub fn for_handle(handle: PersistenceHandle) -> Self {
        Self { handle }
    }

    /// Check the attestation status for an asset given its locator and specification.
    pub fn status(&self, locator: &AssetLocator, spec: &AssetSpec) -> AttestationStatus {
        let key = format!("attestation:loc:{}", locator.to_key_string());
        let reader = match self.handle.reader() {
            Ok(r) => r,
            Err(_) => return AttestationStatus::Quarantined,
        };
        match reader.get_json::<AssetAttestationRecord>(&key) {
            Ok(Some(record)) => {
                let current_fp = spec.fingerprint();
                if record.fingerprint != current_fp {
                    return AttestationStatus::Changed;
                }
                if record.status == AttestationStatus::Denied {
                    return AttestationStatus::Denied;
                }
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                if record.expires_at_s > 0 && now > record.expires_at_s {
                    return AttestationStatus::Expired;
                }
                record.status
            }
            _ => AttestationStatus::Quarantined,
        }
    }

    /// Whether this asset is attested as trusted (or session-ephemeral) and unexpired.
    pub fn is_trusted(&self, locator: &AssetLocator, spec: &AssetSpec) -> bool {
        self.status(locator, spec).is_trusted()
    }

    /// Explicitly trust and record the fingerprint for an asset (granting a 30-day lease).
    pub fn trust_asset(&self, locator: &AssetLocator, spec: &AssetSpec) -> Result<(), String> {
        let fingerprint = spec.fingerprint();
        let key = format!("attestation:loc:{}", locator.to_key_string());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let expires_at_s = now.saturating_add(ATTESTATION_LEASE_TTL_SECS);

        let record = AssetAttestationRecord {
            locator: locator.clone(),
            fingerprint,
            spec: spec.clone(),
            status: AttestationStatus::Trusted,
            trusted_at_s: now,
            expires_at_s,
        };

        self.handle
            .set_json_blocking(&key, &record)
            .map_err(|e| format!("cannot persist asset attestation: {e}"))
    }

    /// Trust an asset with an explicit custom expiration (primarily for testing TTL boundaries).
    pub fn trust_asset_with_expiry(
        &self,
        locator: &AssetLocator,
        spec: &AssetSpec,
        trusted_at_s: u64,
        expires_at_s: u64,
    ) -> Result<(), String> {
        let fingerprint = spec.fingerprint();
        let key = format!("attestation:loc:{}", locator.to_key_string());
        let record = AssetAttestationRecord {
            locator: locator.clone(),
            fingerprint,
            spec: spec.clone(),
            status: AttestationStatus::Trusted,
            trusted_at_s,
            expires_at_s,
        };

        self.handle
            .set_json_blocking(&key, &record)
            .map_err(|e| format!("cannot persist asset attestation: {e}"))
    }

    /// Explicitly quarantine an asset.
    pub fn quarantine_asset(&self, locator: &AssetLocator, spec: &AssetSpec) -> Result<(), String> {
        let fingerprint = spec.fingerprint();
        let key = format!("attestation:loc:{}", locator.to_key_string());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let record = AssetAttestationRecord {
            locator: locator.clone(),
            fingerprint,
            spec: spec.clone(),
            status: AttestationStatus::Quarantined,
            trusted_at_s: now,
            expires_at_s: 0,
        };

        self.handle
            .set_json_blocking(&key, &record)
            .map_err(|e| format!("cannot quarantine asset: {e}"))
    }

    /// Explicitly deny and record the human rejection for an asset (ADR-0253).
    /// Prevents repeated trust gate nagging unless the content changes on disk.
    pub fn deny_asset(&self, locator: &AssetLocator, spec: &AssetSpec) -> Result<(), String> {
        let fingerprint = spec.fingerprint();
        let key = format!("attestation:loc:{}", locator.to_key_string());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let record = AssetAttestationRecord {
            locator: locator.clone(),
            fingerprint,
            spec: spec.clone(),
            status: AttestationStatus::Denied,
            trusted_at_s: now,
            expires_at_s: 0,
        };

        self.handle
            .set_json_blocking(&key, &record)
            .map_err(|e| format!("cannot persist asset denial: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn attestation_ledger_composite_lifecycle_and_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test_assets.db");
        let handle = PersistenceHandle::spawn(db_path, None);
        let ledger = AssetAttestationLedger::for_handle(handle);

        let locator = AssetLocator::UserMcp {
            name: "philpapers".into(),
        };

        let mut env = BTreeMap::new();
        env.insert("PYTHONPATH".into(), "/opt/lib".into());
        let spec_v1 = AssetSpec::Process {
            command: vec!["python3".into(), "-m".into(), "philpapers_mcp".into()],
            env: env.clone(),
        };

        // 1. Initially untrusted / quarantined
        assert_eq!(
            ledger.status(&locator, &spec_v1),
            AttestationStatus::Quarantined
        );
        assert!(!ledger.is_trusted(&locator, &spec_v1));

        // 2. Trust asset -> Trusted with 30d lease
        ledger.trust_asset(&locator, &spec_v1).unwrap();
        assert_eq!(
            ledger.status(&locator, &spec_v1),
            AttestationStatus::Trusted
        );
        assert!(ledger.is_trusted(&locator, &spec_v1));

        // 3. Changed content on the SAME locator -> Status becomes Changed!
        let spec_v2 = AssetSpec::Process {
            command: vec!["python3".into(), "-m".into(), "philpapers_mcp_v2".into()],
            env,
        };
        assert_eq!(
            ledger.status(&locator, &spec_v2),
            AttestationStatus::Changed
        );
        assert!(!ledger.is_trusted(&locator, &spec_v2));

        // 4. Re-trusting with spec_v2 supersedes and restores trust
        ledger.trust_asset(&locator, &spec_v2).unwrap();
        assert_eq!(
            ledger.status(&locator, &spec_v2),
            AttestationStatus::Trusted
        );

        // 5. Expiration boundary test
        ledger
            .trust_asset_with_expiry(&locator, &spec_v2, 1000, 2000)
            .unwrap();
        assert_eq!(
            ledger.status(&locator, &spec_v2),
            AttestationStatus::Expired
        );
        assert!(!ledger.is_trusted(&locator, &spec_v2));

        // 6. Quarantine asset
        ledger.quarantine_asset(&locator, &spec_v2).unwrap();
        assert_eq!(
            ledger.status(&locator, &spec_v2),
            AttestationStatus::Quarantined
        );
        assert!(!ledger.is_trusted(&locator, &spec_v2));

        // 7. Explicit denial (ADR-0253)
        ledger.deny_asset(&locator, &spec_v2).unwrap();
        assert_eq!(ledger.status(&locator, &spec_v2), AttestationStatus::Denied);
        assert!(!ledger.is_trusted(&locator, &spec_v2));
        assert!(ledger.status(&locator, &spec_v2).is_denied());

        // 8. Modifying denied asset invalidates Denial -> Changed!
        let spec_v3 = AssetSpec::Process {
            command: vec!["python3".into(), "-m".into(), "philpapers_mcp_v3".into()],
            env: BTreeMap::new(),
        };
        assert_eq!(
            ledger.status(&locator, &spec_v3),
            AttestationStatus::Changed
        );
    }
}
