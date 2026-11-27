//! Model routing settings and fitted model capability metadata.
use serde::{Deserialize, Serialize};

/// Capability metadata overlaid from a provider's live `GET /models` response
/// for one model id the client registry does not know. Persisted in the
/// remote catalog cache (`remote_catalog.json`) so the metadata survives
/// restarts: a background sync refreshes it, and a failed
/// fetch leaves the last good values in place. Only instances created from a
/// preset whose `RemoteCatalogSource` is an endpoint that returns capability
/// fields (trusted official endpoints) ever carry this.
/// See `nuo_protocol::model::FittedModel`.
#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq, Eq)]
pub struct FittedModelInfo {
    /// Advertised context window in tokens (`0` = the endpoint did not say).
    #[serde(default)]
    pub context_window: usize,
    /// The endpoint advertises reasoning (e.g. a `reasoning_content` stream).
    #[serde(default)]
    pub reasoning: bool,
    /// The endpoint advertises image inputs. `None` (also what an older cache
    /// entry without the field deserializes to) means *undeclared*: the
    /// endpoint said nothing, and the route must not be treated as
    /// text-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    /// Advertised reasoning-effort tiers, as named by the provider.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub efforts: Vec<String>,
}

/// Per-(instance, model) tuning overrides — the user's own per-route
/// choices (set from the model `e` editor), persisted in **state** via
/// [`crate::route_settings::RouteSettingsStore`] (keyed
/// `providers[<instance_id>][<model_id>]`). Unlike the *derived* capability
/// fields ([`FittedModelInfo`]), these are not rebuildable.
///
/// The knobs themselves live in [`TunableSettings`](nuo_protocol::TunableSettings),
/// an **open** map keyed by the provider's own knob ids (ADR-0126). That is
/// what lets a provider ship a new knob without a persistence migration: the
/// map carries it, and a build that has never heard of it still round-trips the
/// value rather than erasing it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RouteSettings {
    /// Every tuning the user has set on this route, keyed by the provider's own
    /// knob id — the **single** carrier of per-route tuning (ADR-0126).
    ///
    /// There are deliberately no typed `effort` / `thinking` fields beside it.
    /// They existed briefly as "mirrors" of two keys in this map, which made one
    /// fact storable two ways and forced every reader to reconcile them. The
    /// map alone is sufficient: depth and the thinking switch are read through
    /// the typed accessors ([`nuo_protocol::TunableSettings::effort`] /
    /// [`nuo_protocol::TunableSettings::thinking`]), and every other provider knob
    /// needs no field at all.
    #[serde(
        default,
        skip_serializing_if = "nuo_protocol::TunableSettings::is_empty"
    )]
    pub tunings: nuo_protocol::TunableSettings,
    /// Explicit capability overrides -- the **top layer** of the capability
    /// resolution order. `None`/empty means "no opinion": the
    /// effective capabilities fall through to remote metadata, then the
    /// static baseline. Unlike the derived `FittedModelInfo`, these are the
    /// user's own per-route choices and are never rebuilt from an endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_overrides: Option<nuo_protocol::CapabilityOverrides>,
    /// Prompt-cache behavior requested for this concrete route. The exact
    /// mode and retention must be advertised by the resolved route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache: Option<nuo_protocol::PromptCachePreference>,
}

impl RouteSettings {
    /// Whether the entry carries any explicit knob. An entry with nothing set
    /// is pruned rather than stored.
    pub fn is_empty(&self) -> bool {
        self.tunings.is_empty()
            && self
                .capability_overrides
                .as_ref()
                .is_none_or(nuo_protocol::CapabilityOverrides::is_empty)
            && self.prompt_cache.is_none()
    }
}
