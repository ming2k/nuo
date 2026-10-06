//! Provider usage querying for Command Code.
//!
//! Queries `GET https://api.commandcode.ai/alpha/billing/credits` with the user's
//! Command Code bearer token to retrieve the credit ledger and the
//! five-hour / weekly window limits, and normalizes them into the generic
//! [`nuo_wire::ProviderUsage`] model.
//!
//! The endpoint is the **subscription server-proxy lane's** own ledger (ADR-0014
//! `[INV-LANE-05]`): the client never computes rate limits or credits itself,
//! it renders exactly what the control plane reports. The response shape is
//! documented in `docs/reference/commandcode-api.yml` (`/alpha/billing/credits`),
//! and this fetcher is a pure projection of it — `windowLimits.fiveHour` /
//! `windowLimits.weekly` become typed [`QuotaWindowBucket`]s, the credit ledger
//! becomes a [`BalanceQuota`], and the resolved `planId` becomes the display
//! badge.

use super::ProviderUsageFetcher;
use async_trait::async_trait;
use nuo_wire::{
    BalanceQuota, ProviderQuotaData, ProviderUsage, QuotaWindowBucket, QuotaWindowKind,
    UsageMetric,
};
use serde::Deserialize;

/// The credit ledger (`/alpha/billing/credits` → `credits`).
#[derive(Debug, Deserialize)]
struct CreditsData {
    #[serde(default, rename = "planId")]
    plan_id: Option<String>,
    #[serde(default, rename = "monthlyCredits")]
    monthly_credits: f64,
    #[serde(default, rename = "purchasedCredits")]
    purchased_credits: f64,
    #[serde(default, rename = "freeCredits")]
    free_credits: f64,
}

/// One rolling window limit (`windowLimits.fiveHour` / `windowLimits.weekly`).
#[derive(Debug, Deserialize)]
struct WindowLimit {
    #[serde(default)]
    used: f64,
    #[serde(default)]
    cap: f64,
    /// Epoch-millisecond clock tick at which this window resets.
    #[serde(default, rename = "resetAt")]
    reset_at: Option<u64>,
}

/// The windowed rate limits (`windowLimits`). `exceeded` is the provider's own
/// reason string for a tripped window (`"5-hour"` / `"weekly"`), else `null`.
#[derive(Debug, Deserialize)]
struct WindowLimits {
    #[serde(default)]
    limited: bool,
    #[serde(default)]
    exceeded: Option<String>,
    #[serde(default, rename = "fiveHour")]
    five_hour: Option<WindowLimit>,
    #[serde(default)]
    weekly: Option<WindowLimit>,
}

#[derive(Debug, Deserialize)]
struct CommandCodeCreditsResponse {
    #[serde(default)]
    credits: Option<CreditsData>,
    #[serde(default, rename = "windowLimits")]
    window_limits: Option<WindowLimits>,
}

pub struct CommandCodeUsageFetcher;

#[async_trait]
impl ProviderUsageFetcher for CommandCodeUsageFetcher {
    fn matches(&self, provider: &str, base_url: &str) -> bool {
        // The canonical provider id is `commandcode-plan` (ADR-0201); the
        // legacy `commandcode` spelling is kept for old stored connections
        // (see `canonical_provider_id`). Matching only the legacy spelling
        // silently disabled usage fetching on every real connection.
        matches!(provider, "commandcode" | "commandcode-plan")
            || base_url.contains("commandcode.ai")
    }

    async fn fetch_usage(
        &self,
        client: &crate::http::Http,
        _base_url: &str,
        api_key: &str,
    ) -> Result<ProviderUsage, String> {
        let endpoint = "https://api.commandcode.ai/alpha/billing/credits";
        let auth = format!("Bearer {api_key}");
        let resp = client
            .get(
                endpoint,
                &[
                    ("authorization", auth.as_str()),
                    ("accept", "application/json"),
                    ("user-agent", crate::NUO_USER_AGENT),
                ],
            )
            .await
            .map_err(|e| format!("HTTP request failed: {e}"))?;

        if !resp.is_success() {
            return Err(format!("HTTP {}: {}", resp.status, resp.body));
        }

        let body: CommandCodeCreditsResponse = serde_json::from_str(&resp.body)
            .map_err(|e| format!("Failed to parse Command Code credits response: {e}"))?;

        Ok(parse_commandcode_credits(body))
    }
}

/// Human-facing badge for a CommandCode `planId`. The plan vocabulary is server
/// data (ADR-0014) rather than client contract data, so an unknown id is
/// humanized rather than dropped.
fn plan_label(plan_id: &str) -> String {
    let trimmed = plan_id.trim();
    if trimmed.is_empty() {
        return "Command Code".to_string();
    }
    // `individual-goat` → `Individual Goat`; `teams-pro` → `Teams Pro`.
    trimmed
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Epoch-millisecond window reset, when reported. The yml types `resetAt` as an
/// integer clock tick; a non-positive value is treated as "not reported".
fn reset_at_ms(raw: Option<u64>) -> Option<u64> {
    raw.filter(|tick| *tick > 0)
}

fn window_bucket(
    label: &str,
    window: QuotaWindowKind,
    limit: WindowLimit,
) -> QuotaWindowBucket {
    let used_fraction = if limit.cap > 0.0 {
        (limit.used / limit.cap).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    QuotaWindowBucket {
        window: Some(window),
        label: label.to_string(),
        group: None,
        used_fraction,
        used_amount: Some(limit.used),
        total_limit: Some(limit.cap),
        unit: Some("credits".to_string()),
        reset_at_ms: reset_at_ms(limit.reset_at),
        reset_time_str: None,
    }
}

/// Project the credits envelope into the generic usage model.
///
/// Infallible: the wire body is already deserialized, and every field is
/// optional, so an empty envelope yields an empty-but-valid projection rather
/// than an error.
fn parse_commandcode_credits(body: CommandCodeCreditsResponse) -> ProviderUsage {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .ok();

    let mut metrics = Vec::new();
    let mut balance = None;
    let mut remaining: Option<f64> = None;
    let mut plan_id: Option<String> = None;

    if let Some(c) = body.credits {
        plan_id = c.plan_id.clone();
        let total = c.monthly_credits + c.purchased_credits + c.free_credits;
        remaining = Some(total);
        balance = Some(BalanceQuota {
            // The CommandCode ledger is denominated in credits, not a fiat
            // currency; the renderer falls back to a bare `{n} credits` form.
            currency: "credits".to_string(),
            total_balance: Some(total),
            cash_balance: (c.purchased_credits > 0.0).then_some(c.purchased_credits),
            voucher_balance: (c.free_credits > 0.0).then_some(c.free_credits),
            credit_limit: None,
            consumed_amount: None,
            display_primary: Some(format!("{total:.2} credits")),
        });
        metrics.push(UsageMetric {
            label: "Monthly Credits".to_string(),
            value: format!("{:.2}", c.monthly_credits),
            unit: Some("credits".to_string()),
        });
        if c.purchased_credits > 0.0 {
            metrics.push(UsageMetric {
                label: "Purchased Credits".to_string(),
                value: format!("{:.2}", c.purchased_credits),
                unit: Some("credits".to_string()),
            });
        }
        if c.free_credits > 0.0 {
            metrics.push(UsageMetric {
                label: "Free Credits".to_string(),
                value: format!("{:.2}", c.free_credits),
                unit: Some("credits".to_string()),
            });
        }
    }

    let mut periodic = None;
    if let Some(wl) = body.window_limits {
        let mut buckets = Vec::new();
        if let Some(fh) = wl.five_hour {
            buckets.push(window_bucket(
                "5-Hour Limit",
                QuotaWindowKind::Rolling5Hour,
                fh,
            ));
        }
        if let Some(w) = wl.weekly {
            buckets.push(window_bucket("Weekly Limit", QuotaWindowKind::Weekly, w));
        }
        if !buckets.is_empty() {
            periodic = Some(nuo_wire::PeriodicQuota { buckets });
        }
        // A tripped window is the provider's own reason string; surface it
        // verbatim as a metric so the alert is visible without parsing it.
        if wl.limited
            && let Some(reason) = wl.exceeded.as_deref().filter(|r| !r.is_empty())
        {
            metrics.push(UsageMetric {
                label: "Window Limit".to_string(),
                value: format!("exceeded ({reason})"),
                unit: None,
            });
        }
    }

    let quota = if balance.is_some() || periodic.is_some() {
        Some(ProviderQuotaData::Composite {
            balance,
            periodic,
            rate_limits: Vec::new(),
        })
    } else {
        None
    };

    ProviderUsage {
        plan: Some(
            plan_id
                .as_deref()
                .map(plan_label)
                .unwrap_or_else(|| "Command Code".to_string()),
        ),
        description: Some("Command Code API credits".to_string()),
        quota,
        primary_balance: remaining.map(|r| format!("{r:.2} credits")),
        metrics,
        updated_at_ms: now_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::ProviderUsageFetcher;

    #[test]
    fn matches_canonical_and_legacy_provider_ids_and_base_url() {
        assert!(CommandCodeUsageFetcher.matches("commandcode-plan", "https://api.commandcode.ai/provider/v1"));
        assert!(CommandCodeUsageFetcher.matches("commandcode", "https://api.commandcode.ai/provider/v1"));
        assert!(CommandCodeUsageFetcher.matches("anything", "https://api.commandcode.ai/provider/v1"));
        assert!(!CommandCodeUsageFetcher.matches("deepseek", "https://api.deepseek.com/v1"));
    }

    #[test]
    fn parses_goat_ledger_into_a_composite_quota() {
        let json = r#"{
            "credits": {
                "planId": "individual-goat",
                "belowThreshold": false,
                "creditThreshold": 0,
                "monthlyCredits": 69.597485352,
                "purchasedCredits": 0,
                "freeCredits": 0
            },
            "windowLimits": {
                "limited": true,
                "exceeded": null,
                "fiveHour": { "used": 0.402514648, "cap": 14, "exceeded": false, "resetAt": 1791119912775 },
                "weekly": { "used": 0.402514648, "cap": 35, "exceeded": false, "resetAt": 1791706712775 }
            },
            "sandboxAccess": false,
            "sandboxMinutes": null
        }"#;
        let parsed: CommandCodeCreditsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.credits.as_ref().unwrap().plan_id.as_deref(), Some("individual-goat"));

        let usage = parse_commandcode_credits(parsed);
        assert_eq!(usage.plan.as_deref(), Some("Individual Goat"));

        let Some(ProviderQuotaData::Composite {
            balance,
            periodic,
            rate_limits,
        }) = usage.quota
        else {
            panic!("expected Composite quota");
        };
        assert!(rate_limits.is_empty());

        let bal = balance.expect("credit ledger");
        assert_eq!(bal.currency, "credits");
        assert_eq!(bal.total_balance, Some(69.597485352));
        // Zero purchased/free credits must not render an empty sub-row.
        assert_eq!(bal.cash_balance, None);
        assert_eq!(bal.voucher_balance, None);

        let per = periodic.expect("window limits");
        assert_eq!(per.buckets.len(), 2);
        assert_eq!(per.buckets[0].window, Some(QuotaWindowKind::Rolling5Hour));
        assert_eq!(per.buckets[0].total_limit, Some(14.0));
        assert_eq!(per.buckets[0].reset_at_ms, Some(1791119912775));
        assert!((per.buckets[0].used_fraction - 0.0287).abs() < 0.001);
        assert_eq!(per.buckets[1].window, Some(QuotaWindowKind::Weekly));
        assert_eq!(per.buckets[1].total_limit, Some(35.0));

        // No window tripped → no "Window Limit" metric.
        assert!(!usage.metrics.iter().any(|m| m.label == "Window Limit"));
    }

    #[test]
    fn surfaces_the_providers_tripped_window_reason_verbatim() {
        let json = r#"{
            "credits": { "monthlyCredits": 1.0, "purchasedCredits": 0, "freeCredits": 0 },
            "windowLimits": {
                "limited": true,
                "exceeded": "5-hour",
                "fiveHour": { "used": 14.0, "cap": 14, "exceeded": true, "resetAt": 1791119912775 }
            }
        }"#;
        let parsed: CommandCodeCreditsResponse = serde_json::from_str(json).unwrap();
        let usage = parse_commandcode_credits(parsed);
        let metric = usage
            .metrics
            .iter()
            .find(|m| m.label == "Window Limit")
            .expect("tripped window metric");
        assert_eq!(metric.value, "exceeded (5-hour)");
    }

    #[test]
    fn zero_cap_window_is_not_a_divide_by_zero() {
        let json = r#"{
            "windowLimits": { "limited": false, "exceeded": null, "fiveHour": { "used": 0, "cap": 0, "resetAt": 0 } }
        }"#;
        let parsed: CommandCodeCreditsResponse = serde_json::from_str(json).unwrap();
        let usage = parse_commandcode_credits(parsed);
        let Some(ProviderQuotaData::Composite { periodic, .. }) = usage.quota else {
            panic!("expected Composite quota");
        };
        let per = periodic.expect("window limits");
        assert_eq!(per.buckets[0].used_fraction, 0.0);
        assert_eq!(per.buckets[0].reset_at_ms, None);
    }

    #[test]
    fn humanizes_unknown_plan_ids() {
        assert_eq!(plan_label("individual-goat"), "Individual Goat");
        assert_eq!(plan_label("teams-pro"), "Teams Pro");
        assert_eq!(plan_label(""), "Command Code");
    }
}
