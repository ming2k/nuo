//! Connection detail and provider usage / quota models.

use serde::{Deserialize, Serialize};

/// Time window categorization for periodic rate limits / quotas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaWindowKind {
    /// 5-hour rolling limit window (e.g. Antigravity / Claude / Codex).
    Rolling5Hour,
    /// Daily 24-hour quota window.
    Daily,
    /// Weekly quota window.
    Weekly,
    /// Monthly tier quota window.
    Monthly,
    /// Custom interval window.
    Custom,
}

impl QuotaWindowKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Rolling5Hour => "5h Window",
            Self::Daily => "Daily",
            Self::Weekly => "Weekly",
            Self::Monthly => "Monthly",
            Self::Custom => "Custom Window",
        }
    }
}

/// Detailed state of one periodic quota bucket / window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct QuotaWindowBucket {
    /// Window type (5h rolling, daily, weekly, monthly, custom).
    pub window: Option<QuotaWindowKind>,
    /// Human-friendly label (e.g. "Gemini 3.7 Flash", "5h Rolling Limit", "Daily Budget").
    pub label: String,
    /// Logical group / model family this bucket belongs to (e.g. "Chat Models", "Claude Models").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Used fraction from 0.0 (0% used) to 1.0 (100% depleted).
    pub used_fraction: f32,
    /// Optional absolute used amount (e.g. 15 requests, 12000 tokens).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_amount: Option<f64>,
    /// Optional absolute total limit amount (e.g. 50 requests, 100000 tokens).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_limit: Option<f64>,
    /// Unit for amounts (e.g. "requests", "tokens", "USD").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Epoch milliseconds when this quota window resets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_at_ms: Option<u64>,
    /// Raw reset time string if parsed from provider (e.g. "2026-09-01T12:00:00Z").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_time_str: Option<String>,
}

/// A set of periodic quota buckets (e.g. per-model or multi-window combinations).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PeriodicQuota {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub buckets: Vec<QuotaWindowBucket>,
}

/// Balance, credits, and spending limit details for pay-as-you-go or prepaid accounts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct BalanceQuota {
    /// Currency code (e.g. "CNY", "USD") or credit unit.
    pub currency: String,
    /// Available total balance (for prepaid accounts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_balance: Option<f64>,
    /// Cash / topped-up / recharge balance component.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cash_balance: Option<f64>,
    /// Voucher / granted / gift balance component.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voucher_balance: Option<f64>,
    /// Credit limit ceiling (e.g. OpenRouter limit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit_limit: Option<f64>,
    /// Consumed amount against the credit limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumed_amount: Option<f64>,
    /// Formatted primary balance string for display (e.g. "¥100.50", "$1.23 / $10.00").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_primary: Option<String>,
}

/// Concurrency and request / token rate limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RateLimitSpec {
    pub requests: i64,
    pub interval: String,
}

/// Typed classification of a provider's quota / billing architecture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderQuotaData {
    /// Pure periodic window limits (e.g. Antigravity, Claude, ChatGPT subscription).
    Periodic(PeriodicQuota),
    /// Pure prepaid / pay-as-you-go balance (e.g. DeepSeek, Kimi, SiliconFlow).
    Balance(BalanceQuota),
    /// Hybrid: combined periodic windows, balances, and/or rate limits (e.g. OpenRouter).
    Composite {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        balance: Option<BalanceQuota>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        periodic: Option<PeriodicQuota>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        rate_limits: Vec<RateLimitSpec>,
    },
}

/// Generic normalized provider usage / quota / balance info.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ProviderUsage {
    /// High-level plan / account tier badge (e.g. "Google AI Premium", "Pay-as-you-go", "Tier 2").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// Optional descriptive text or policy note from provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Typed structured quota / balance data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<ProviderQuotaData>,
    /// Primary balance / credit summary (e.g. "¥10.00", "$5.20", "10.00 CNY").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_balance: Option<String>,
    /// Structured usage metrics / breakdown.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metrics: Vec<UsageMetric>,
    /// Unix epoch milliseconds of when this usage data was retrieved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at_ms: Option<u64>,
}

/// One named metric in a provider's usage / quota report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageMetric {
    /// Metric label, e.g. "Total Balance", "Granted Balance", "Rate Limit".
    pub label: String,
    /// Metric value, e.g. "¥10.00", "200 req / 10s", "0.50".
    pub value: String,
    /// Optional unit or currency, e.g. "CNY", "USD", "requests".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

/// State of a connection's usage / quota retrieval.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", content = "data", rename_all = "snake_case")]
pub enum ConnectionUsageState {
    /// Provider does not support remote usage querying.
    #[default]
    Unsupported,
    /// Usage query is currently in progress.
    Fetching,
    /// Usage data retrieved successfully.
    Available(Box<ProviderUsage>),
    /// Usage query failed with an error message.
    Error(String),
}

/// Full inspection detail for one connection in the `/connections` modal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ConnectionDetail {
    /// Connection name — the connection's identity (ADR-0201).
    pub name: String,
    /// The model provider this connection points at (e.g. "deepseek", "anthropic").
    pub provider: String,
    /// Human-friendly provider label (e.g. "DeepSeek", "Anthropic").
    pub provider_label: String,
    /// Wire protocol label (e.g. "openai", "anthropic", "google").
    pub protocol: String,
    /// Base URL endpoint.
    pub base_url: String,
    /// Authentication type description (e.g. "API Key", "OAuth").
    pub auth_type: String,
    /// Masked API key preview for security (e.g. "sk-12...abcd"), or None if no key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_masked: Option<String>,
    /// Where the credential originates (e.g. "credentials.toml", "Environment (DEEPSEEK_API_KEY)").
    pub api_key_source: String,
    /// Client identity configuration.
    pub client_identity: crate::ClientIdentity,
    /// Effective User-Agent string sent with requests.
    pub user_agent: String,
    /// Every model served by this connection.
    #[serde(default)]
    pub models: Vec<String>,
    /// Detailed model settings for served models in the same order as `models`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_info: Vec<crate::ProviderModelInfo>,
    /// Active / default model for this connection if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_model: Option<String>,
    /// Active / default model reasoning effort if configured (e.g. "high", "medium", "max").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_model_effort: Option<String>,
    /// Active / default model thinking state if configured (e.g. true for adaptive thinking).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_model_thinking: Option<bool>,
    /// Remote provider usage / quota / balance state.
    pub usage: ConnectionUsageState,
}
