//! Provider usage querying for Command Code.
//!
//! Queries `GET https://api.commandcode.ai/alpha/billing/credits` with the user's
//! Command Code bearer token to retrieve the credit ledger and the
//! five-hour / weekly window limits, and normalizes them into the generic
//! [`nuo_protocol::ProviderUsage`] model.
//!
//! This is the authoritative fetcher (the `nuo-provider-adapters` duplicate was retired by ADR-0027). It is
//! a pure projection of the control plane's answer (ADR-0014 `[INV-LANE-05]`):
//! the client renders the reported credits and windows verbatim and never
//! recomputes either. Response shape: `docs/reference/openapi/commandcode-goat.yml`
//! (`/alpha/billing/credits`).
use nuo_protocol::{
    BalanceQuota, PeriodicQuota, ProviderQuotaData, ProviderUsage, QuotaWindowBucket,
    QuotaWindowKind, UsageMetric,
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
    /// The control plane's own verdict that *this* window has tripped. This is
    /// the trip signal — never `windowLimits.limited`, which is a property of
    /// the *plan* (a windowed plan reports `limited: true` even while nothing
    /// has tripped, which is exactly the healthy `individual-goat` benchmark).
    #[serde(default)]
    exceeded: bool,
    /// Epoch-millisecond clock tick (`docs/reference/openapi/commandcode-goat.yml`
    /// `WindowLimit.resetAt`).
    #[serde(default, rename = "resetAt")]
    reset_at: Option<u64>,
}

/// The windowed rate limits (`windowLimits`).
///
/// `windowLimits.limited` is deliberately not modelled: it describes the *plan's*
/// shape, not whether a window has tripped, so a healthy windowed plan reports
/// `true`. The per-window `exceeded` flag is the trip signal.
#[derive(Debug, Deserialize)]
struct WindowLimits {
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

impl CommandCodeUsageFetcher {
    pub fn matches(&self, provider: &str, base_url: &str) -> bool {
        provider == "commandcode-goat" || base_url.contains("commandcode.ai")
    }

    pub async fn fetch_usage(
        &self,
        client: &nuo_provider::transport::Http,
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
                    ("user-agent", nuo_provider::NUO_USER_AGENT),
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

/// Human-facing badge for a CommandCode `planId`. Plan ids are server data
/// (ADR-0014), so an unknown id is humanized rather than dropped.
fn plan_label(plan_id: &str) -> String {
    let trimmed = plan_id.trim();
    if trimmed.is_empty() {
        return String::new();
    }
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

/// A reported `resetAt` tick, kept only when present and non-zero.
///
/// The value is already an epoch-millisecond clock tick, so it is passed
/// through unchanged (`[INV-LANE-05]`: the client never rescales the control
/// plane's answer).
fn reset_at_ms(raw: Option<u64>) -> Option<u64> {
    raw.filter(|tick| *tick > 0)
}

/// Whether the control plane reports this window as tripped.
///
/// `exceeded` is authoritative. The used fraction is a defensive floor for a
/// server that answers at 100% without flipping the flag; a healthy window at
/// 0.4 / 14 credits is never mistaken for an exhausted one.
fn window_tripped(limit: &WindowLimit) -> bool {
    limit.exceeded || (limit.cap > 0.0 && (limit.used / limit.cap) >= 0.999)
}

/// The provider's own "window exhausted" rule, replayed verbatim.
///
/// The reset tick is the window's reported `resetAt` (epoch **milliseconds**);
/// scaling it as seconds pinned the countdown ~56 000 years into the future, so
/// an expired limit never re-enabled its models.
fn exhausted_window_rule(
    status_label: &str,
    reason: &str,
    reset_at: Option<u64>,
) -> nuo_protocol::ModelAvailabilityRule {
    nuo_protocol::ModelAvailabilityRule {
        pattern: "*".to_string(),
        availability: nuo_protocol::Availability::disabled(
            status_label,
            Some(reason.to_string()),
            reset_at_ms(reset_at),
        ),
    }
}

fn window_bucket(label: &str, window: QuotaWindowKind, limit: WindowLimit) -> QuotaWindowBucket {
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
    let mut model_rules = Vec::new();
    if let Some(wl) = body.window_limits {
        let mut buckets = Vec::new();
        if let Some(fh) = wl.five_hour {
            // `limited` is the plan's shape; the window has *tripped* only when
            // the control plane says so. Treating `limited` as the trip signal
            // disabled every model on a healthy windowed plan.
            if window_tripped(&fh) {
                model_rules.push(exhausted_window_rule(
                    "disabled: 5h limit",
                    "5h rolling quota limit reached for Command Code",
                    fh.reset_at,
                ));
            }
            buckets.push(window_bucket(
                "5-Hour Limit",
                QuotaWindowKind::Rolling5Hour,
                fh,
            ));
        }
        if let Some(w) = wl.weekly {
            if window_tripped(&w) {
                model_rules.push(exhausted_window_rule(
                    "disabled: weekly quota",
                    "Weekly quota limit reached for Command Code",
                    w.reset_at,
                ));
            }
            buckets.push(window_bucket("Weekly Limit", QuotaWindowKind::Weekly, w));
        }
        if !buckets.is_empty() {
            periodic = Some(PeriodicQuota { buckets });
        }
        // The server names *which* window tripped (e.g. `"5-hour"` / `"weekly"`)
        // and is surfaced verbatim, never parsed into a local decision.
        if let Some(reason) = wl.exceeded.as_deref().filter(|r| !r.is_empty()) {
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
        plan: plan_id.as_deref().map(plan_label).filter(|s| !s.is_empty()),
        description: Some("Command Code API credits".to_string()),
        quota,
        primary_balance: remaining.map(|r| format!("{r:.2} credits")),
        metrics,
        model_rules,
        updated_at_ms: now_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> ProviderUsage {
        let body: CommandCodeCreditsResponse = serde_json::from_str(json).expect("wire body");
        parse_commandcode_credits(body)
    }

    fn window_buckets(usage: &ProviderUsage) -> &[QuotaWindowBucket] {
        match usage.quota.as_ref() {
            Some(ProviderQuotaData::Composite {
                periodic: Some(p), ..
            }) => &p.buckets,
            other => panic!("expected a composite quota with periodic windows, got {other:?}"),
        }
    }

    // The live `individual-goat` benchmark (ADR-0014): a windowed plan reports
    // `limited: true` while nothing has tripped. That must not disable models,
    // and the reported `resetAt` is already epoch milliseconds.
    #[test]
    fn healthy_windowed_plan_never_disables_models() {
        let usage = parse(
            r#"{
                "credits": { "planId": "individual-goat", "monthlyCredits": 69.597485352,
                             "purchasedCredits": 0, "freeCredits": 0 },
                "windowLimits": {
                    "limited": true, "exceeded": null,
                    "fiveHour": { "used": 0.402514648, "cap": 14, "exceeded": false,
                                  "resetAt": 1791119912775 },
                    "weekly":   { "used": 0.402514648, "cap": 35, "exceeded": false,
                                  "resetAt": 1791706712775 }
                }
            }"#,
        );

        assert!(
            usage.model_rules.is_empty(),
            "a healthy plan must keep its models usable: {:?}",
            usage.model_rules
        );
        assert_eq!(usage.plan.as_deref(), Some("Individual Goat"));
        assert_eq!(usage.primary_balance.as_deref(), Some("69.60 credits"));

        let buckets = window_buckets(&usage);
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].window, Some(QuotaWindowKind::Rolling5Hour));
        assert_eq!(buckets[1].window, Some(QuotaWindowKind::Weekly));
        // `resetAt` is milliseconds: rescaling it as seconds produced a tick
        // ~56 000 years in the future.
        assert_eq!(buckets[0].reset_at_ms, Some(1791119912775));
        assert_eq!(buckets[1].reset_at_ms, Some(1791706712775));
        assert!((buckets[0].used_fraction - 0.028751).abs() < 1e-5);
    }

    #[test]
    fn tripped_window_disables_models_with_a_millisecond_reset() {
        let usage = parse(
            r#"{
                "windowLimits": {
                    "limited": true, "exceeded": "5-hour",
                    "fiveHour": { "used": 14, "cap": 14, "exceeded": true,
                                  "resetAt": 1791119912775 }
                }
            }"#,
        );

        assert_eq!(usage.model_rules.len(), 1);
        let rule = &usage.model_rules[0];
        assert_eq!(rule.pattern, "*");
        assert!(!rule.availability.usable);
        assert_eq!(
            rule.availability.status_label.as_deref(),
            Some("disabled: 5h limit")
        );
        assert_eq!(rule.availability.resets_at_ms, Some(1791119912775));
        assert!(
            usage
                .metrics
                .iter()
                .any(|m| { m.label == "Window Limit" && m.value == "exceeded (5-hour)" })
        );
    }

    // A capped window whose `exceeded` flag was not flipped is still exhausted.
    #[test]
    fn a_window_at_cap_is_exhausted_even_without_the_flag() {
        let usage = parse(
            r#"{
                "windowLimits": {
                    "limited": true, "exceeded": null,
                    "weekly": { "used": 35, "cap": 35, "exceeded": false, "resetAt": 0 }
                }
            }"#,
        );
        assert_eq!(usage.model_rules.len(), 1);
        assert_eq!(
            usage.model_rules[0].availability.status_label.as_deref(),
            Some("disabled: weekly quota")
        );
        // A zero tick is "no reported reset", never epoch 0.
        assert_eq!(usage.model_rules[0].availability.resets_at_ms, None);
    }
}
