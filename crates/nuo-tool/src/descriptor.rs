//! Tool metadata as **data** (ADR-0008 `[INV-TOOL-09]`).
//!
//! A tool's static facts — its identity, schema, risk, scopes, and
//! capability flags — are fields of one [`ToolDescriptor`] struct rather than
//! ~15 methods on the tool trait. This makes the toolset **inspectable,
//! serializable, and auditable as data** before any tool is instantiated:
//! duplicate-identity checks, safe-target audits, permission-coverage reports,
//! and model-capability filtering all read descriptors, not live instances.
//!
//! The descriptor is the canonical metadata vocabulary for the single tool
//! contract. Tools build one (via [`ToolDescriptor::builder`]) and return it
//! from `Tool::descriptor`.

use serde::{Deserialize, Serialize};

use crate::risk::RiskProfile;
use crate::scope::ToolScope;

/// Static, inspectable metadata for one tool implementation.
///
/// Every field has a sensible default so adding a new field never breaks
/// existing tool implementations (the method-explosion churn ADR-0008 removes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDescriptor {
    /// Distinct identifier of the tool (e.g. `read_text`). Shared by all
    /// variants of one capability.
    pub name: String,
    /// Natural-language description of what the tool does.
    pub description: String,
    /// JSON Schema describing accepted arguments (a draft-07 object schema).
    pub parameters_schema: serde_json::Value,
    /// The variant id distinguishing this implementation from other variants
    /// of the same capability. Never reaches the model; it is the selection
    /// key for per-model and per-profile variant pinning.
    pub variant: String,
    /// Compatibility aliases that may resolve to this tool during dispatch.
    pub aliases: Vec<String>,
    /// Declared risk profile for policy reasoning.
    pub risk: RiskProfile,
    /// Declared operational scopes for stage-gated assembly.
    pub scopes: Vec<ToolScope>,
    /// Whether executing this tool may block awaiting a live human decision.
    pub requires_user: bool,
    /// Whether this tool only functions on a model that can perceive images.
    pub requires_vision: bool,
    /// Whether invoking this tool spawns a nested sub-agent.
    pub spawns_subagent: bool,
    /// Whether this tool exercises control over the harness itself (e.g. an
    /// abort/exit escape hatch) rather than the workspace/filesystem.
    pub affects_control_flow: bool,
    /// Whether this tool is currently available/configured and should be
    /// admitted to model requests.
    pub available: bool,
}

impl Default for ToolDescriptor {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            parameters_schema: serde_json::json!({ "type": "object" }),
            variant: "default".to_string(),
            aliases: Vec::new(),
            risk: RiskProfile::ReadOnly,
            scopes: Vec::new(),
            requires_user: false,
            requires_vision: false,
            spawns_subagent: false,
            affects_control_flow: false,
            available: true,
        }
    }
}

impl ToolDescriptor {
    /// Start building a descriptor for a named tool.
    pub fn builder(name: impl Into<String>, description: impl Into<String>) -> ToolDescriptorBuilder {
        ToolDescriptorBuilder {
            descriptor: ToolDescriptor {
                name: name.into(),
                description: description.into(),
                ..ToolDescriptor::default()
            },
        }
    }

    /// Whether this descriptor matches a requested dispatch name (exact name
    /// or an alias).
    pub fn matches_name(&self, requested: &str) -> bool {
        self.name == requested || self.aliases.iter().any(|a| a == requested)
    }

    /// The `(name, variant)` identity used for duplicate-registration checks.
    pub fn identity(&self) -> (&str, &str) {
        (self.name.as_str(), self.variant.as_str())
    }
}

/// Fluent builder for [`ToolDescriptor`]. Keeps tool definitions one expression
/// deep without a struct literal of a dozen fields.
#[derive(Debug, Clone)]
pub struct ToolDescriptorBuilder {
    descriptor: ToolDescriptor,
}

impl ToolDescriptorBuilder {
    /// Set the JSON-schema for the tool's arguments.
    pub fn schema(mut self, schema: serde_json::Value) -> Self {
        self.descriptor.parameters_schema = schema;
        self
    }

    /// Set the variant id.
    pub fn variant(mut self, variant: impl Into<String>) -> Self {
        self.descriptor.variant = variant.into();
        self
    }

    /// Set the compatibility aliases.
    pub fn aliases(mut self, aliases: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.descriptor.aliases = aliases.into_iter().map(Into::into).collect();
        self
    }

    /// Set the risk profile.
    pub fn risk(mut self, risk: RiskProfile) -> Self {
        self.descriptor.risk = risk;
        self
    }

    /// Set the declared operational scopes.
    pub fn scopes(mut self, scopes: impl IntoIterator<Item = ToolScope>) -> Self {
        self.descriptor.scopes = scopes.into_iter().collect();
        self
    }

    /// Mark whether this tool may block awaiting a live human.
    pub fn requires_user(mut self, yes: bool) -> Self {
        self.descriptor.requires_user = yes;
        self
    }

    /// Mark whether this tool needs a vision-capable model.
    pub fn requires_vision(mut self, yes: bool) -> Self {
        self.descriptor.requires_vision = yes;
        self
    }

    /// Mark whether this tool spawns a nested sub-agent.
    pub fn spawns_subagent(mut self, yes: bool) -> Self {
        self.descriptor.spawns_subagent = yes;
        self
    }

    /// Mark whether this tool exercises control over the harness itself.
    pub fn affects_control_flow(mut self, yes: bool) -> Self {
        self.descriptor.affects_control_flow = yes;
        self
    }

    /// Mark whether this tool is currently available.
    pub fn available(mut self, yes: bool) -> Self {
        self.descriptor.available = yes;
        self
    }

    /// Freeze the descriptor.
    pub fn build(self) -> ToolDescriptor {
        self.descriptor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let d = ToolDescriptor::default();
        assert_eq!(d.variant, "default");
        assert_eq!(d.risk, RiskProfile::ReadOnly);
        assert!(d.available);
        assert!(!d.spawns_subagent);
        assert_eq!(d.identity(), ("", "default"));
    }

    #[test]
    fn builder_composes_metadata_as_data() {
        let d = ToolDescriptor::builder("write_file", "Writes a file")
            .schema(serde_json::json!({"type": "object"}))
            .variant("terse")
            .aliases(["write"])
            .risk(RiskProfile::IdempotentMutation)
            .scopes([ToolScope::Workspace])
            .build();
        assert_eq!(d.name, "write_file");
        assert_eq!(d.identity(), ("write_file", "terse"));
        assert!(d.matches_name("write_file"));
        assert!(d.matches_name("write"));
        assert!(!d.matches_name("read_text"));
        assert_eq!(d.risk, RiskProfile::IdempotentMutation);
    }

    #[test]
    fn descriptor_is_serializable_as_data() {
        let d = ToolDescriptor::builder("read_text", "Reads a file").build();
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["name"], "read_text");
        let round: ToolDescriptor = serde_json::from_value(json).unwrap();
        assert_eq!(round, d);
    }
}
