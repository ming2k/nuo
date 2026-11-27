//! Per-route tuning **storage** — the open envelope a provider-owned knob lives
//! in (ADR-0126).
//!
//! This module is deliberately small. It answers exactly one question: *how does
//! a per-route tuning choice travel and persist without the framework knowing
//! what the knob means?* It does not define knob shapes, declarative specs, or
//! renderers — there is no consumer for those, and inventing them would be a
//! road with no traffic (see the ADR's negative knowledge).
//!
//! # What the framework owns, and what it does not
//!
//! | Question | Owner |
//! |---|---|
//! | Which knobs does a route expose? | **provider** — `Model` declarations (`effort_levels`, `thinking`) |
//! | What values does a knob accept? | **provider** — its own ladder |
//! | What does an absent choice mean? | **provider** — `Model::default_effort` |
//! | How does a chosen value reach the wire? | **provider** — its own request builder |
//! | How is a choice stored and carried? | **framework** — this module |
//! | What does the UI show? | **framework** — resolved server-side, rendered verbatim |
//!
//! # The preservation rule
//!
//! [`TunableSetting::Rung`] (a verbatim wire string) plus the untyped
//! `BTreeMap` envelope mean an
//! unrecognised knob round-trips through storage and IPC untouched. A client
//! that cannot render a knob keeps its value rather than erasing it. Without
//! this, one un-upgraded client would silently delete a newer provider's
//! setting — the exact failure mode provider-owned configuration exists to
//! avoid.
//!
//! # Why only two knob ids are named here
//!
//! [`EFFORT_KNOB`] and [`THINKING_KNOB`] are constants because the framework
//! models *depth* and *the on/off switch* as first-class concepts (`Effort`,
//! `ReasoningMode`). Every other knob id is the provider's own string and is
//! never matched outside its owning crate. See ADR-0126 for the promotion gate a
//! knob must clear to become framework-owned.

use std::collections::BTreeMap;

use crate::effort::Effort;

/// The knob id for reasoning **depth**.
///
/// Framework-owned because depth has a total order (so a request can be clamped),
/// a shared cross-vendor vocabulary (so a caption is written once), and a
/// default rule (so a route with no stored choice still sends something). Every
/// other knob id belongs to the provider that declared it.
pub const EFFORT_KNOB: &str = "effort";

/// The knob id for the extended-thinking on/off switch.
///
/// Framework-owned because "is reasoning on at all" is orthogonal to "how deep",
/// and the framework models that orthogonality ([`crate::reasoning::ReasoningMode`]).
/// A provider that exposes no separate switch simply never sets this key.
pub const THINKING_KNOB: &str = "thinking";

/// A user's chosen value for one tunable on one route.
///
/// Untagged on the wire: a toggle serializes as `true`, a rung as its string.
/// The pair (unknown map key, verbatim string) is what lets a knob the receiving
/// build has never heard of survive a round-trip: the key is preserved because
/// the map is open, and the value because [`Self::Rung`] carries the raw wire
/// string rather than a framework type.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum TunableSetting {
    /// A toggle's state.
    Flag(bool),
    /// A depth ladder or named-choice rung, as its wire string.
    ///
    /// This is also how an **unrecognised** provider value survives: the string
    /// is carried verbatim, and the map key is kept, so a build that has no name
    /// for the knob still round-trips it. That is why no third "opaque JSON"
    /// variant exists — a string plus an open key already expresses every knob
    /// the envelope can honestly carry.
    Rung(String),
}

impl TunableSetting {
    /// The value as a string, when it has one. A toggle has none — it is a
    /// boolean, not a named rung.
    pub fn as_rung(&self) -> Option<&str> {
        match self {
            Self::Rung(rung) => Some(rung),
            Self::Flag(_) => None,
        }
    }

    /// The toggle state, when this is one.
    pub const fn as_flag(&self) -> Option<bool> {
        match self {
            Self::Flag(flag) => Some(*flag),
            Self::Rung(_) => None,
        }
    }

    /// The rung as a framework-vocabulary depth, when it names one.
    pub fn as_known_effort(&self) -> Option<Effort> {
        self.as_rung().and_then(Effort::parse)
    }
}

impl From<bool> for TunableSetting {
    fn from(flag: bool) -> Self {
        Self::Flag(flag)
    }
}

impl From<&str> for TunableSetting {
    fn from(rung: &str) -> Self {
        Self::Rung(rung.to_string())
    }
}

impl From<String> for TunableSetting {
    fn from(rung: String) -> Self {
        Self::Rung(rung)
    }
}

impl From<Effort> for TunableSetting {
    fn from(effort: Effort) -> Self {
        Self::Rung(effort.as_str().to_string())
    }
}

/// Every tunable the user has set on one route, keyed by the provider's own
/// knob id.
///
/// This is the **single writable shape** for per-route tuning: storage persists
/// it and IPC carries it. Adding a provider knob requires no change here — the
/// map is open by construction, which is the whole point. A typed accessor
/// exists for the two knobs whose semantics the framework itself owns (depth and
/// the thinking switch); everything else is read through [`Self::get`].
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct TunableSettings(BTreeMap<String, TunableSetting>);

impl TunableSettings {
    /// An empty set — "the user has expressed no tuning choices on this route".
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }

    /// Whether the user has set nothing at all.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The user's value for `id`, if they set one.
    pub fn get(&self, id: &str) -> Option<&TunableSetting> {
        self.0.get(id)
    }

    /// Set `id` to `value`, returning the previous value.
    pub fn set(&mut self, id: impl Into<String>, value: TunableSetting) -> Option<TunableSetting> {
        self.0.insert(id.into(), value)
    }

    /// Remove `id`, returning the previous value.
    pub fn remove(&mut self, id: &str) -> Option<TunableSetting> {
        self.0.remove(id)
    }

    /// Every `(knob id, value)` pair, in stable key order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &TunableSetting)> {
        self.0.iter().map(|(key, value)| (key.as_str(), value))
    }

    /// The user's reasoning-depth choice, when they made one that names a
    /// framework rung.
    pub fn effort(&self) -> Option<Effort> {
        self.get(EFFORT_KNOB)
            .and_then(TunableSetting::as_known_effort)
    }

    /// The user's explicit extended-thinking switch, when they set one.
    pub fn thinking(&self) -> Option<bool> {
        self.get(THINKING_KNOB).and_then(TunableSetting::as_flag)
    }

    /// Replace the reasoning-depth choice. `None` clears it (fall through to
    /// the provider's declared default).
    pub fn set_effort(&mut self, effort: Option<Effort>) {
        match effort {
            Some(effort) => {
                self.set(EFFORT_KNOB, TunableSetting::from(effort));
            }
            None => {
                self.remove(EFFORT_KNOB);
            }
        }
    }

    /// Replace the extended-thinking switch. `None` clears it.
    pub fn set_thinking(&mut self, thinking: Option<bool>) {
        match thinking {
            Some(flag) => {
                self.set(THINKING_KNOB, TunableSetting::from(flag));
            }
            None => {
                self.remove(THINKING_KNOB);
            }
        }
    }
}

impl<'a> IntoIterator for &'a TunableSettings {
    type Item = (&'a str, &'a TunableSetting);
    type IntoIter = Box<dyn Iterator<Item = (&'a str, &'a TunableSetting)> + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.0.iter().map(|(key, value)| (key.as_str(), value)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_unknown_knobs_verbatim() {
        // The preservation rule: a knob (and value) this build has never heard
        // of must survive serialization untouched, so an older frontend cannot
        // erase a newer provider's setting.
        let mut settings = TunableSettings::new();
        settings.set("edit_format", TunableSetting::from("apply_patch"));
        settings.set("thinking", TunableSetting::Flag(false));
        settings.set_effort(Some(Effort::Xhigh));
        // An unrecognised rung for an unrecognised knob — the real shape of a
        // provider tier this build cannot name.
        settings.set("safety_blocks", TunableSetting::from("hash:h1"));

        let json = serde_json::to_string(&settings).expect("settings must serialize");
        let back: TunableSettings = serde_json::from_str(&json).expect("settings must parse");

        assert_eq!(back, settings);
        assert_eq!(
            back.get("edit_format").and_then(|v| v.as_rung()),
            Some("apply_patch")
        );
        assert_eq!(back.thinking(), Some(false));
        assert_eq!(back.effort(), Some(Effort::Xhigh));
        let reemitted = serde_json::to_value(&back).expect("settings must re-serialize");
        let object = reemitted
            .as_object()
            .expect("a settings map is a JSON object");
        assert_eq!(object.len(), 4, "no knob may be dropped on round-trip");
    }

    #[test]
    fn settings_serialize_as_a_bare_map_with_untagged_values() {
        // Transparent map + untagged values: a toggle is a JSON bool, a rung is
        // a JSON string. That is what makes the envelope readable by a build
        // that knows none of the knob ids.
        let mut settings = TunableSettings::new();
        settings.set_thinking(Some(true));
        settings.set_effort(Some(Effort::Low));
        assert_eq!(
            serde_json::to_value(&settings).expect("settings must serialize"),
            serde_json::json!({"thinking": true, "effort": "low"})
        );
    }

    #[test]
    fn clearing_a_knob_restores_fall_through() {
        let mut settings = TunableSettings::new();
        settings.set_effort(Some(Effort::Max));
        settings.set_thinking(Some(false));
        assert!(!settings.is_empty());

        settings.set_effort(None);
        settings.set_thinking(None);
        assert!(settings.is_empty(), "cleared knobs leave no residue");
        assert_eq!(settings.effort(), None);
        assert_eq!(settings.thinking(), None);
    }

    #[test]
    fn unrecognized_rung_is_not_a_framework_effort() {
        // A rung the vocabulary does not name is still stored and stamped;
        // it is simply not rankable.
        let setting = TunableSetting::from("turbo");
        assert_eq!(setting.as_rung(), Some("turbo"));
        assert_eq!(setting.as_known_effort(), None);
        assert_eq!(setting.as_flag(), None);
    }
}
