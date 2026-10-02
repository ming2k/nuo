//! Route-scoped prompt-cache capabilities, preferences, and telemetry.
//!
//! Cache behavior belongs to one concrete provider route: endpoint, wire
//! protocol, dialect, and model generation together determine which controls
//! are valid. Protocol resemblance never grants cache support to a relay.

use serde::{Deserialize, Serialize};

/// A request-visible cache placement mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptCacheMode {
    /// The upstream caches eligible prefixes without a request control.
    Implicit,
    /// A top-level control advances the cache boundary with the conversation.
    Automatic,
    /// The client marks cache boundaries on concrete content blocks.
    Explicit,
}

/// An exact provider retention control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheRetention {
    InMemory,
    FiveMinutes,
    ThirtyMinutes,
    OneHour,
    TwentyFourHours,
}

/// The requested cache mode for one route.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptCacheModePreference {
    /// Use the route's declared default mode.
    #[default]
    ProviderDefault,
    Implicit,
    Automatic,
    Explicit,
    /// Require the route to suppress cache writes.
    Disabled,
}

/// User or caller intent for one model request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PromptCachePreference {
    pub mode: PromptCacheModePreference,
    /// `None` keeps the route's declared default retention.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention: Option<CacheRetention>,
}

/// Prompt-cache support of one concrete provider route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptCacheCapabilities {
    /// Every mode the route accepts. Empty means unsupported.
    pub modes: Vec<PromptCacheMode>,
    /// The mode used for `ProviderDefault`. Required when `modes` is nonempty.
    pub default_mode: Option<PromptCacheMode>,
    /// Exact retention values accepted by this route.
    pub supported_retentions: Vec<CacheRetention>,
    /// Retention used when the request does not choose one.
    pub default_retention: Option<CacheRetention>,
    /// Whether an explicit disabled plan can be enforced on the wire.
    pub disable_supported: bool,
    /// Whether a stable affinity key may accompany the selected mode.
    pub routing_key_supported: bool,
    /// Maximum explicit breakpoints accepted per request.
    pub max_breakpoints: Option<u8>,
    pub min_cacheable_tokens: Option<u32>,
    pub reports_reads: bool,
    pub reports_writes: bool,
    pub reports_misses: bool,
}

/// Const-friendly route declaration embedded in provider registries.
#[derive(Debug, Clone, Copy)]
pub struct PromptCacheSpec {
    pub modes: &'static [PromptCacheMode],
    pub default_mode: Option<PromptCacheMode>,
    pub supported_retentions: &'static [CacheRetention],
    pub default_retention: Option<CacheRetention>,
    pub disable_supported: bool,
    pub routing_key_supported: bool,
    pub max_breakpoints: Option<u8>,
    pub min_cacheable_tokens: Option<u32>,
    pub reports_reads: bool,
    pub reports_writes: bool,
    pub reports_misses: bool,
}

impl PromptCacheSpec {
    pub const UNSUPPORTED: Self = Self {
        modes: &[],
        default_mode: None,
        supported_retentions: &[],
        default_retention: None,
        disable_supported: false,
        routing_key_supported: false,
        max_breakpoints: None,
        min_cacheable_tokens: None,
        reports_reads: false,
        reports_writes: false,
        reports_misses: false,
    };

    #[allow(clippy::expect_used)] // Static route declarations must fail fast when internally invalid.
    pub fn materialize(self) -> PromptCacheCapabilities {
        let capabilities = PromptCacheCapabilities {
            modes: self.modes.to_vec(),
            default_mode: self.default_mode,
            supported_retentions: self.supported_retentions.to_vec(),
            default_retention: self.default_retention,
            disable_supported: self.disable_supported,
            routing_key_supported: self.routing_key_supported,
            max_breakpoints: self.max_breakpoints,
            min_cacheable_tokens: self.min_cacheable_tokens,
            reports_reads: self.reports_reads,
            reports_writes: self.reports_writes,
            reports_misses: self.reports_misses,
        };
        capabilities
            .validate()
            .expect("invalid static prompt-cache route declaration");
        capabilities
    }
}

impl PromptCacheCapabilities {
    pub const fn unsupported() -> Self {
        Self {
            modes: Vec::new(),
            default_mode: None,
            supported_retentions: Vec::new(),
            default_retention: None,
            disable_supported: false,
            routing_key_supported: false,
            max_breakpoints: None,
            min_cacheable_tokens: None,
            reports_reads: false,
            reports_writes: false,
            reports_misses: false,
        }
    }

    pub fn validate(&self) -> Result<(), CacheResolutionError> {
        match (self.modes.is_empty(), self.default_mode) {
            (true, None) => {}
            (true, Some(_)) => return Err(CacheResolutionError::InvalidCapabilities),
            (false, Some(mode)) if self.modes.contains(&mode) => {}
            (false, _) => return Err(CacheResolutionError::InvalidCapabilities),
        }
        if self
            .default_retention
            .is_some_and(|value| !self.supported_retentions.contains(&value))
        {
            return Err(CacheResolutionError::InvalidCapabilities);
        }
        if self.modes.contains(&PromptCacheMode::Explicit)
            && self.max_breakpoints.is_none_or(|value| value == 0)
        {
            return Err(CacheResolutionError::InvalidCapabilities);
        }
        Ok(())
    }

    pub fn resolve(
        &self,
        preference: PromptCachePreference,
        routing_key: Option<String>,
    ) -> Result<ResolvedCachePolicy, CacheResolutionError> {
        self.validate()?;
        if self.modes.is_empty() {
            return match preference {
                PromptCachePreference {
                    mode: PromptCacheModePreference::ProviderDefault,
                    retention: None,
                } => Ok(ResolvedCachePolicy::Unsupported),
                _ => Err(CacheResolutionError::CachingUnsupported),
            };
        }

        if preference.mode == PromptCacheModePreference::Disabled {
            return if self.disable_supported {
                Ok(ResolvedCachePolicy::Disabled)
            } else {
                Err(CacheResolutionError::DisableUnsupported)
            };
        }

        let mode = match preference.mode {
            PromptCacheModePreference::ProviderDefault => self
                .default_mode
                .ok_or(CacheResolutionError::InvalidCapabilities)?,
            PromptCacheModePreference::Implicit => PromptCacheMode::Implicit,
            PromptCacheModePreference::Automatic => PromptCacheMode::Automatic,
            PromptCacheModePreference::Explicit => PromptCacheMode::Explicit,
            PromptCacheModePreference::Disabled => unreachable!("handled above"),
        };
        if !self.modes.contains(&mode) {
            return Err(CacheResolutionError::ModeUnsupported { mode });
        }

        let retention = preference.retention.or(self.default_retention);
        if let Some(value) = retention
            && !self.supported_retentions.contains(&value)
        {
            return Err(CacheResolutionError::RetentionUnsupported { retention: value });
        }

        Ok(ResolvedCachePolicy::Enabled {
            mode,
            retention,
            routing_key: self.routing_key_supported.then_some(routing_key).flatten(),
            max_breakpoints: (mode == PromptCacheMode::Explicit)
                .then_some(self.max_breakpoints)
                .flatten(),
        })
    }
}

/// Fully validated cache instructions consumed by a protocol encoder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ResolvedCachePolicy {
    Unsupported,
    Disabled,
    Enabled {
        mode: PromptCacheMode,
        retention: Option<CacheRetention>,
        routing_key: Option<String>,
        max_breakpoints: Option<u8>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheResolutionError {
    InvalidCapabilities,
    CachingUnsupported,
    DisableUnsupported,
    ModeUnsupported { mode: PromptCacheMode },
    RetentionUnsupported { retention: CacheRetention },
}

impl std::fmt::Display for CacheResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidCapabilities => write!(f, "invalid prompt-cache route capabilities"),
            Self::CachingUnsupported => {
                write!(f, "this provider route exposes no prompt-cache controls")
            }
            Self::DisableUnsupported => {
                write!(f, "this provider route cannot disable prompt caching")
            }
            Self::ModeUnsupported { mode } => {
                write!(f, "this provider route does not support {mode:?} caching")
            }
            Self::RetentionUnsupported { retention } => write!(
                f,
                "this provider route does not support {retention:?} cache retention"
            ),
        }
    }
}

impl std::error::Error for CacheResolutionError {}

/// Provider-neutral cache telemetry parsed from an upstream usage object.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptCacheUsage {
    pub read_tokens: i64,
    pub write_tokens: i64,
    pub miss_tokens: Option<i64>,
}

/// Parse all supported upstream cache counters without conflating absent,
/// zero, and miss telemetry.
pub fn read_prompt_cache_usage(usage: &serde_json::Value) -> PromptCacheUsage {
    let read_tokens = first_non_negative(&[
        usage["cached_tokens"].as_i64(),
        usage["prompt_tokens_details"]["cached_tokens"].as_i64(),
        usage["input_tokens_details"]["cached_tokens"].as_i64(),
        usage["cachedContentTokenCount"].as_i64(),
        usage["prompt_cache_hit_tokens"].as_i64(),
        usage["cache_read_input_tokens"].as_i64(),
    ])
    .unwrap_or(0);
    let write_tokens = first_non_negative(&[
        usage["input_tokens_details"]["cache_write_tokens"].as_i64(),
        usage["cache_creation_input_tokens"].as_i64(),
    ])
    .unwrap_or(0);
    let miss_tokens = first_non_negative(&[usage["prompt_cache_miss_tokens"].as_i64()]);

    PromptCacheUsage {
        read_tokens,
        write_tokens,
        miss_tokens,
    }
}

fn first_non_negative(values: &[Option<i64>]) -> Option<i64> {
    values.iter().flatten().copied().find(|value| *value >= 0)
}

/// Provider-neutral cache plan derived from a prepared request (ADR-0217).
///
/// It carries the deterministic content identity of the cacheable prefix
/// `S | H | I` and separates the durable conversation from the request-local
/// temporary context `E`. Breakpoint placement, block sizing, retention, and
/// affinity stay provider concerns applied on top of this plan; this type makes
/// no claim that a cache hit occurs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachePlan {
    /// Stable content identity of the cacheable prefix. Excludes `E`.
    pub prefix_fingerprint: String,
    /// Number of `S | H | I` messages carried by the request.
    pub conversation_messages: usize,
    /// Number of `E` messages, excluded from the prefix and never persisted.
    pub temporary_context_messages: usize,
}

impl CachePlan {
    pub fn from_request(request: &crate::ModelRequest) -> Self {
        Self {
            prefix_fingerprint: crate::request_prefix_fingerprint(
                &request.instructions,
                &request.messages,
                &request.tool_specs,
            ),
            conversation_messages: request.messages.len(),
            temporary_context_messages: request.temporary_context.len(),
        }
    }

    /// Whether the request-local temporary context is empty. An empty tail
    /// cannot shorten a later prefix.
    pub fn has_temporary_context(&self) -> bool {
        self.temporary_context_messages > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn provider_default_uses_the_declared_default_not_feature_order() {
        let caps = PromptCacheCapabilities {
            modes: vec![PromptCacheMode::Implicit, PromptCacheMode::Explicit],
            default_mode: Some(PromptCacheMode::Implicit),
            supported_retentions: vec![CacheRetention::ThirtyMinutes],
            default_retention: Some(CacheRetention::ThirtyMinutes),
            disable_supported: false,
            routing_key_supported: true,
            max_breakpoints: Some(4),
            min_cacheable_tokens: Some(1024),
            reports_reads: true,
            reports_writes: true,
            reports_misses: false,
        };
        assert_eq!(
            caps.resolve(PromptCachePreference::default(), Some("session-42".into())),
            Ok(ResolvedCachePolicy::Enabled {
                mode: PromptCacheMode::Implicit,
                retention: Some(CacheRetention::ThirtyMinutes),
                routing_key: Some("session-42".into()),
                max_breakpoints: None,
            })
        );
    }

    #[test]
    fn unsupported_route_rejects_claimed_controls() {
        let caps = PromptCacheCapabilities::unsupported();
        assert_eq!(
            caps.resolve(PromptCachePreference::default(), None),
            Ok(ResolvedCachePolicy::Unsupported)
        );
        assert_eq!(
            caps.resolve(
                PromptCachePreference {
                    mode: PromptCacheModePreference::Explicit,
                    retention: None,
                },
                None
            ),
            Err(CacheResolutionError::CachingUnsupported)
        );
    }

    #[test]
    fn rejects_unadvertised_retention() {
        let caps = PromptCacheCapabilities {
            modes: vec![PromptCacheMode::Automatic],
            default_mode: Some(PromptCacheMode::Automatic),
            supported_retentions: vec![CacheRetention::FiveMinutes],
            default_retention: Some(CacheRetention::FiveMinutes),
            disable_supported: true,
            routing_key_supported: false,
            max_breakpoints: None,
            min_cacheable_tokens: None,
            reports_reads: true,
            reports_writes: true,
            reports_misses: false,
        };
        assert_eq!(
            caps.resolve(
                PromptCachePreference {
                    mode: PromptCacheModePreference::ProviderDefault,
                    retention: Some(CacheRetention::OneHour),
                },
                None
            ),
            Err(CacheResolutionError::RetentionUnsupported {
                retention: CacheRetention::OneHour
            })
        );
    }

    #[test]
    fn parses_openai_read_and_write_counters() {
        assert_eq!(
            read_prompt_cache_usage(&json!({
                "input_tokens_details": {
                    "cached_tokens": 800,
                    "cache_write_tokens": 200,
                }
            })),
            PromptCacheUsage {
                read_tokens: 800,
                write_tokens: 200,
                miss_tokens: None,
            }
        );
    }

    #[test]
    fn parses_deepseek_hit_and_miss_counters() {
        assert_eq!(
            read_prompt_cache_usage(&json!({
                "prompt_cache_hit_tokens": 600,
                "prompt_cache_miss_tokens": 400,
            })),
            PromptCacheUsage {
                read_tokens: 600,
                write_tokens: 0,
                miss_tokens: Some(400),
            }
        );
    }

    #[test]
    fn cache_plan_is_deterministic_for_an_unchanged_request() {
        let request =
            crate::ModelRequest::new(vec![crate::Message::new(crate::Role::User, "hello")]);
        assert_eq!(request.cache_plan(), request.cache_plan());
    }

    #[test]
    fn cache_plan_identity_ignores_the_temporary_tail() {
        let conversation = vec![crate::Message::new(crate::Role::User, "hello")];
        let without = crate::ModelRequest::new(conversation.clone());
        let with_tail = crate::ModelRequest::new(conversation).with_temporary_context(vec![
            crate::Message::new(crate::Role::User, "request-local"),
        ]);

        let bare = without.cache_plan();
        let tailed = with_tail.cache_plan();
        assert_eq!(
            bare.prefix_fingerprint, tailed.prefix_fingerprint,
            "request-local temporary context must not move the cacheable prefix identity"
        );
        assert_eq!(bare.conversation_messages, 1);
        assert_eq!(tailed.conversation_messages, 1);
        assert_eq!(tailed.temporary_context_messages, 1);
        assert!(!bare.has_temporary_context());
        assert!(tailed.has_temporary_context());
    }

    #[test]
    fn cache_plan_identity_tracks_conversation_content() {
        let a = crate::ModelRequest::new(vec![crate::Message::new(crate::Role::User, "a")])
            .cache_plan();
        let b = crate::ModelRequest::new(vec![crate::Message::new(crate::Role::User, "b")])
            .cache_plan();
        assert_ne!(a.prefix_fingerprint, b.prefix_fingerprint);
    }
}
