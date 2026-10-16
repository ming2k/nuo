//! Connection-level dual-mode budget governance (ADR-0065, ADR-0066).
//!
//! Delineates between remote-enforced providers (upstream controls quota/rejection)
//! and local-enforced budgets (client-side pre-flight spend caps) (`[INV-BUDGET-01]`).

use serde::{Deserialize, Serialize};
use crate::pricing::NanoUsd;

/// Scope of the local spend counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BudgetPeriod {
    /// Budget cap applies to the active interactive session.
    #[default]
    Session,
    /// Budget cap applies to the local calendar day across all sessions.
    Daily,
    /// Budget cap applies to total cumulative spend recorded on this connection.
    Total,
}

impl BudgetPeriod {
    pub fn label(self) -> &'static str {
        match self {
            Self::Session => "Session",
            Self::Daily => "Daily",
            Self::Total => "Total",
        }
    }
}

/// Connection-level budget governance policy (`[INV-BUDGET-01]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ConnectionBudget {
    /// Remote-managed: upstream controls rate-limits and balances; client lets requests proceed
    /// until upstream returns an authentic refusal (402, 429).
    #[default]
    Remote,
    /// Local-enforced: client tracks accurate cumulative spend and halts execution when limit is reached.
    Local {
        /// Budget ceiling in USD (deserialized from float or int, represented as NanoUsd) (`[INV-PRICING-02]`).
        #[serde(with = "usd_nano_serde")]
        limit: NanoUsd,
        /// Primary currency code, defaults to "USD".
        #[serde(default = "default_currency")]
        currency: String,
        /// Accounting scope/period for the cap.
        #[serde(default)]
        period: BudgetPeriod,
    },
}

fn default_currency() -> String {
    "USD".to_string()
}

mod usd_nano_serde {
    use super::*;
    use serde::{de, Deserializer, Serializer};

    pub fn serialize<S>(value: &NanoUsd, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_f64(value.to_usd_f64())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<NanoUsd, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct UsdVisitor;
        impl<'de> de::Visitor<'de> for UsdVisitor {
            type Value = NanoUsd;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a float or integer representing USD amount")
            }

            fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(NanoUsd::from_usd_f64(v))
            }

            fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(NanoUsd::from_usd_f64(v as f64))
            }

            fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(NanoUsd::from_usd_f64(v as f64))
            }
        }

        deserializer.deserialize_any(UsdVisitor)
    }
}

impl ConnectionBudget {
    /// Create a local budget policy with specified limit in USD.
    pub fn local(limit: f64, period: BudgetPeriod) -> Self {
        Self::Local {
            limit: NanoUsd::from_usd_f64(limit),
            currency: "USD".to_string(),
            period,
        }
    }

    /// Whether this connection uses local client-side budget enforcement.
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local { .. })
    }

    /// Returns the budget limit in [`NanoUsd`] if locally enforced.
    pub fn limit_nano_usd(&self) -> Option<NanoUsd> {
        match self {
            Self::Remote => None,
            Self::Local { limit, .. } => Some(*limit),
        }
    }

    /// Returns the budget period if locally enforced.
    pub fn period(&self) -> Option<BudgetPeriod> {
        match self {
            Self::Remote => None,
            Self::Local { period, .. } => Some(*period),
        }
    }

    /// Check if current accumulated spend has met or exceeded the local budget limit (`[INV-BREAKER-01]`).
    pub fn is_exceeded(&self, accumulated: NanoUsd) -> bool {
        match self.limit_nano_usd() {
            Some(limit) => accumulated >= limit,
            None => false,
        }
    }
}
