use super::*;
use nuo_tool::{ScopeTarget, ToolAccesses};

use async_trait::async_trait;
use std::path::PathBuf;

struct PingTool;
#[async_trait]
impl Tool for PingTool {
    fn name(&self) -> &str {
        "registry_ping"
    }
    fn description(&self) -> &str {
        "test tool"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    async fn call(&self, _arguments: &str) -> Result<String, String> {
        Ok("pong".to_string())
    }
}

struct DeclinedTool;
#[async_trait]
impl Tool for DeclinedTool {
    fn name(&self) -> &str {
        "registry_declined"
    }
    fn description(&self) -> &str {
        "declined test tool"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    async fn call(&self, _arguments: &str) -> Result<String, String> {
        Ok("never".to_string())
    }
}

register_tool!(PingFactory => PingTool);
// Declines when a (deliberately absent) marker service is missing.
register_tool!(DeclinedFactory => |ctx| {
    let _ = ctx.get::<String>()?;
    DeclinedTool
});

/// A well-behaved write tool: declares a write AND overrides scope_target to
/// surface the path it touches. Must pass `debug_assert_safe_targets`.
struct SafeWriteTool;
#[async_trait]
impl Tool for SafeWriteTool {
    fn name(&self) -> &str {
        "safe_write"
    }
    fn description(&self) -> &str {
        "test write tool with a correct scope_target"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn call(&self, _arguments: &str) -> Result<String, String> {
        Ok("wrote".to_string())
    }
    fn scope_target(&self, arguments: &str) -> ScopeTarget {
        // Surface the path so the scope-gate / broker can check it.
        serde_json::from_str::<serde_json::Value>(arguments)
            .ok()
            .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(PathBuf::from))
            .map(ScopeTarget::Path)
            .unwrap_or(ScopeTarget::Unspecified)
    }
    // accesses is conditional in lockstep with scope_target: a write is only
    // declared when there is a real path to write — the realistic shape.
    fn accesses(&self, arguments: &str) -> ToolAccesses {
        match self.scope_target(arguments) {
            ScopeTarget::Path(p) => ToolAccesses::write_file(p.to_string_lossy().into_owned()),
            _ => ToolAccesses::none(),
        }
    }
}

/// A *misbehaving* write tool: declares a write but leaves scope_target at
/// the `Unspecified` default. Must trip `debug_assert_safe_targets`.
struct UnsafeWriteTool;
#[async_trait]
impl Tool for UnsafeWriteTool {
    fn name(&self) -> &str {
        "unsafe_write"
    }
    fn description(&self) -> &str {
        "test write tool that forgets scope_target"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn call(&self, _arguments: &str) -> Result<String, String> {
        Ok("wrote".to_string())
    }
    // scope_target defaults to Unspecified — the hole.
    fn accesses(&self, _arguments: &str) -> ToolAccesses {
        ToolAccesses::write_file("/sample".to_string())
    }
}

#[cfg(debug_assertions)]
#[test]
fn safe_targets_passes_for_correct_write_tool() {
    let tool: Arc<dyn Tool> = Arc::new(SafeWriteTool);
    // Must not panic: the write tool surfaces its target.
    debug_assert_safe_targets(std::slice::from_ref(&tool));
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "would bypass the")]
fn safe_targets_flags_write_tool_with_unspecified_target() {
    let tool: Arc<dyn Tool> = Arc::new(UnsafeWriteTool);
    // Must panic: the tool declares a write but returns Unspecified, so the
    // write would bypass every permission gate.
    debug_assert_safe_targets(std::slice::from_ref(&tool));
}

#[cfg(debug_assertions)]
#[test]
fn safe_targets_ignores_read_only_tools() {
    // A read-only tool (no declared write) may legitimately use the
    // Unspecified default — its auto-admit path.
    let tool: Arc<dyn Tool> = Arc::new(PingTool);
    debug_assert_safe_targets(std::slice::from_ref(&tool));
}

#[test]
fn collect_toolset_includes_registered_and_skips_declined() {
    let ctx = ToolFactoryContextBuilder::new().build();
    let toolset = collect_toolset(&ctx);
    let names: Vec<&str> = toolset
        .capability_names()
        .filter(|name| name.starts_with("registry_"))
        .collect();
    assert!(names.contains(&"registry_ping"));
    assert!(!names.contains(&"registry_declined"));
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "duplicate builtin tool registration")]
fn duplicate_identity_panics_in_debug() {
    // Same name AND same (default) variant => a true identity collision.
    debug_assert_unique_identities(&[
        Arc::new(PingTool) as Arc<dyn Tool>,
        Arc::new(PingTool) as Arc<dyn Tool>,
    ]);
}

#[cfg(debug_assertions)]
#[test]
fn distinct_variants_are_not_a_collision() {
    // Same name but different variant is the legitimate variant mechanism,
    // not a double-register; the guard must allow it.
    debug_assert_unique_identities(&[
        Arc::new(VariantTool {
            variant: "default",
            desc: "default impl",
        }) as Arc<dyn Tool>,
        Arc::new(VariantTool {
            variant: "terse",
            desc: "terse impl",
        }) as Arc<dyn Tool>,
    ]);
}

/// Two variants of one capability: `name()` shared, `variant()` distinct.
struct VariantTool {
    variant: &'static str,
    desc: &'static str,
}
#[async_trait]
impl Tool for VariantTool {
    fn name(&self) -> &str {
        "cap"
    }
    fn variant(&self) -> &str {
        self.variant
    }
    fn description(&self) -> &str {
        self.desc
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    async fn call(&self, _arguments: &str) -> Result<String, String> {
        Ok(String::new())
    }
}

fn toolset_with_variants() -> ToolSet {
    ToolSet::from_tools(vec![
        Arc::new(VariantTool {
            variant: "default",
            desc: "default impl",
        }) as Arc<dyn Tool>,
        Arc::new(VariantTool {
            variant: "terse",
            desc: "terse impl",
        }) as Arc<dyn Tool>,
    ])
}

#[test]
fn from_tools_groups_variants_under_one_capability() {
    let toolset = toolset_with_variants();
    assert_eq!(toolset.len(), 1);
    let cap = toolset.variants_of("cap").unwrap();
    assert_eq!(cap.default_variant(), "default");
    let ids: Vec<&str> = cap.variant_ids().collect();
    assert_eq!(ids, vec!["default", "terse"]);
}

#[test]
fn resolve_picks_selected_variant_else_default() {
    let toolset = toolset_with_variants();

    // No selection → default variant.
    let resolved = toolset.resolve(empty_variant_selection());
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].description(), "default impl");

    // Selection names the terse variant → terse impl.
    let mut selection = VariantSelection::new();
    selection.insert("cap".to_string(), "terse".to_string());
    let resolved = toolset.resolve(&selection);
    assert_eq!(resolved[0].description(), "terse impl");

    // Selection names a missing variant → falls back to default.
    let mut selection = VariantSelection::new();
    selection.insert("cap".to_string(), "nope".to_string());
    let resolved = toolset.resolve(&selection);
    assert_eq!(resolved[0].description(), "default impl");
}

#[test]
fn upsert_replaces_one_concrete_identity() {
    let mut toolset = toolset_with_variants();
    let replaced = toolset.upsert(Arc::new(VariantTool {
        variant: "default",
        desc: "agent-owned impl",
    }));

    assert_eq!(
        replaced.as_deref().map(Tool::description),
        Some("default impl")
    );
    assert_eq!(toolset.default_view()[0].description(), "agent-owned impl");
    assert_eq!(
        toolset
            .variants_of("cap")
            .expect("capability should remain installed")
            .variant_or_default(Some("terse"))
            .description(),
        "terse impl"
    );
}

#[test]
fn default_falls_back_to_smallest_id_when_no_default_named() {
    let toolset = ToolSet::from_tools(vec![
        Arc::new(VariantTool {
            variant: "zulu",
            desc: "z",
        }) as Arc<dyn Tool>,
        Arc::new(VariantTool {
            variant: "alpha",
            desc: "a",
        }) as Arc<dyn Tool>,
    ]);
    let cap = toolset.variants_of("cap").unwrap();
    assert_eq!(cap.default_variant(), "alpha");
}

/// A tool with a configurable name, variant, and vision requirement, for
/// exercising `resolve_for`'s scope/override/model-capability composition.
struct CapTool {
    name: &'static str,
    variant: &'static str,
    requires_vision: bool,
}
#[async_trait]
impl Tool for CapTool {
    fn name(&self) -> &str {
        self.name
    }
    fn variant(&self) -> &str {
        self.variant
    }
    fn description(&self) -> &str {
        self.variant
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    fn requires_vision(&self) -> bool {
        self.requires_vision
    }
    async fn call(&self, _arguments: &str) -> Result<String, String> {
        Ok(String::new())
    }
}

fn cap(name: &'static str, variant: &'static str, requires_vision: bool) -> Arc<dyn Tool> {
    Arc::new(CapTool {
        name,
        variant,
        requires_vision,
    })
}

/// A test model with vision on/off (other fields are irrelevant to the
/// resolver).
fn model(vision: bool) -> Model {
    Model {
        id: "test",
        family: "test",
        context_window: 100_000,
        thinking: crate::reasoning::ReasoningSupport::None,
        tool_call: true,
        vision,
        protocol: crate::WireProtocol::OpenAiChatCompletions,
        effort_levels: &[],
        default_effort: Some(crate::Effort::High),
        wire_entities: None,
    }
}

#[test]
fn tool_scope_intersect_is_a_meet() {
    // All is the identity.
    let only = CapabilityScope::only(["a", "b"]);
    assert_eq!(CapabilityScope::All.intersect(&only), only);
    assert_eq!(only.intersect(&CapabilityScope::All), only);
    // Two Only sets intersect.
    let other = CapabilityScope::only(["b", "c"]);
    assert_eq!(only.intersect(&other), CapabilityScope::only(["b"]));
    // Commutative.
    assert_eq!(only.intersect(&other), other.intersect(&only));
}

#[test]
fn resolve_for_intersects_agent_and_model_scope() {
    let pool = ToolSet::from_tools(vec![
        cap("read", "default", false),
        cap("search_text", "default", false),
        cap("write_file", "default", false),
    ]);
    // Agent admits read + write_file; model admits everything (All).
    let agent = ToolSelection::only(["read", "write_file"]);
    let model_sel = ToolSelection::unrestricted();
    let resolved = pool.resolve_for(&model(true), &agent, &model_sel);
    let names: Vec<&str> = resolved.iter().map(|t| t.name()).collect();
    assert_eq!(names, vec!["read", "write_file"]);
}

#[test]
fn resolve_for_drops_vision_tool_on_text_only_model() {
    // `view_image` is a single-variant, vision-only capability.
    let pool = ToolSet::from_tools(vec![
        cap("read", "default", false),
        cap("view_image", "default", true),
    ]);
    let agent = ToolSelection::unrestricted();
    let model_sel = ToolSelection::unrestricted();

    // Vision model: both survive.
    let resolved = pool.resolve_for(&model(true), &agent, &model_sel);
    let names: Vec<&str> = resolved.iter().map(|t| t.name()).collect();
    assert_eq!(names, vec!["read", "view_image"]);

    // Text-only model: view_image is dropped entirely — the capability has
    // no model-usable variant.
    let resolved = pool.resolve_for(&model(false), &agent, &model_sel);
    let names: Vec<&str> = resolved.iter().map(|t| t.name()).collect();
    assert_eq!(names, vec!["read"]);
}

#[test]
fn resolve_for_model_falls_back_to_usable_variant_over_unusable_default() {
    // One capability with a vision default + a text-only variant: a
    // text-only model must get the usable variant, not lose the capability.
    let pool = ToolSet::from_tools(vec![
        cap("describe", "default", true), // vision default
        cap("describe", "text", false),   // text-only fallback
    ]);
    let agent = ToolSelection::unrestricted();
    let model_sel = ToolSelection::unrestricted();

    // Text-only model: default is unusable, so the resolver picks `text`.
    let resolved = pool.resolve_for(&model(false), &agent, &model_sel);
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].variant(), "text");

    // Vision model: keeps the default.
    let resolved = pool.resolve_for(&model(true), &agent, &model_sel);
    assert_eq!(resolved[0].variant(), "default");
}

#[test]
fn resolve_for_agent_override_beats_model_override() {
    // Capability with two model-usable variants. Agent pins `terse`, model
    // pins `verbose`; agent precedence must win.
    let pool = ToolSet::from_tools(vec![
        cap("read", "default", false),
        cap("read", "terse", false),
        cap("read", "verbose", false),
    ]);
    let mut agent = ToolSelection::unrestricted();
    agent
        .variants
        .insert("read".to_string(), "terse".to_string());
    let mut model_sel = ToolSelection::unrestricted();
    model_sel
        .variants
        .insert("read".to_string(), "verbose".to_string());

    let resolved = pool.resolve_for(&model(true), &agent, &model_sel);
    assert_eq!(resolved[0].variant(), "terse");
}

#[test]
fn resolve_for_model_override_fills_unpinned_capability() {
    // Agent pins nothing; the model's override supplies the variant.
    let pool = ToolSet::from_tools(vec![
        cap("read", "default", false),
        cap("read", "verbose", false),
    ]);
    let agent = ToolSelection::unrestricted();
    let mut model_sel = ToolSelection::unrestricted();
    model_sel
        .variants
        .insert("read".to_string(), "verbose".to_string());

    let resolved = pool.resolve_for(&model(true), &agent, &model_sel);
    assert_eq!(resolved[0].variant(), "verbose");
}

#[test]
fn resolve_for_model_cannot_be_overridden_into_an_unusable_variant() {
    // The hard-limit invariant: even if the agent explicitly pins the
    // vision variant, a text-only model never receives it.
    let pool = ToolSet::from_tools(vec![
        cap("describe", "text", false),
        cap("describe", "vision", true),
    ]);
    let mut agent = ToolSelection::unrestricted();
    agent
        .variants
        .insert("describe".to_string(), "vision".to_string());
    let model_sel = ToolSelection::unrestricted();

    let resolved = pool.resolve_for(&model(false), &agent, &model_sel);
    assert_eq!(resolved.len(), 1);
    assert_eq!(
        resolved[0].variant(),
        "text",
        "agent override must not reinstate a model-unusable variant"
    );
}

#[test]
fn context_provides_and_looks_up_by_type() {
    let mut builder = ToolFactoryContextBuilder::new();
    builder.provide(42_u32).provide(String::from("hi"));
    let ctx = builder.build();
    assert_eq!(ctx.get::<u32>(), Some(&42));
    assert_eq!(ctx.get::<String>().map(String::as_str), Some("hi"));
    assert_eq!(ctx.get::<bool>(), None);
}

#[test]
fn shared_clones_an_arc_out() {
    let mut builder = ToolFactoryContextBuilder::new();
    builder.provide(Arc::new(7_i32));
    let ctx = builder.build();
    let handle: Arc<i32> = ctx.shared::<i32>().unwrap();
    assert_eq!(*handle, 7);
    assert!(ctx.shared::<u64>().is_none());
}
#[test]
fn workspace_roots_admission_and_visibility() {
    use super::WorkspaceRoots;
    use std::path::Path;

    let roots = WorkspaceRoots::new("/proj/frontend", vec!["/proj/backend".into()]);
    // Primary keeps its resolution duties.
    assert_eq!(roots.primary(), Path::new("/proj/frontend"));
    assert!(roots.is_multi_root());
    // Admission spans every root...
    assert!(roots.contains(Path::new("/proj/frontend/src/main.rs")));
    assert!(roots.contains(Path::new("/proj/backend/api.rs")));
    // ...and fails closed elsewhere.
    assert!(!roots.contains(Path::new("/proj/design-kit/logo.png")));
    assert!(!roots.contains(Path::new("/etc/passwd")));
    // Surfacing: primary first, then additional in declaration order.
    let listed: Vec<&Path> = roots.iter().collect();
    assert_eq!(
        listed,
        vec![Path::new("/proj/frontend"), Path::new("/proj/backend")]
    );

    // The single-root construction admits exactly the primary.
    let single = WorkspaceRoots::single("/proj/frontend");
    assert!(!single.is_multi_root());
    assert!(single.contains(Path::new("/proj/frontend/x")));
    assert!(!single.contains(Path::new("/proj/backend/x")));
}

#[test]
fn tool_pool_declaration_and_resolution() {
    let pool = ToolPool::from_tools(vec![
        cap("read", "default", false),
        cap("search_text", "default", false),
        cap("write_file", "default", false),
    ]);

    let decl = ToolDeclaration::new(
        "code_analyst",
        crate::AgentKind::Root,
        CapabilityScope::only(["read", "search_text"]),
    );
    pool.declare(decl.clone());

    assert_eq!(pool.get_declaration("code_analyst"), Some(decl));
    assert_eq!(pool.get_declaration("unknown"), None);

    let agent = ToolSelection::only(["read", "search_text"]);
    let resolved = pool.resolve_for(&model(true), &agent, &ToolSelection::unrestricted());
    let names: Vec<&str> = resolved.iter().map(|t| t.name()).collect();
    assert_eq!(names, vec!["read", "search_text"]);

    let snapshot = pool.snapshot(&agent, &model(true));
    assert_eq!(snapshot.capabilities.len(), 3);
    let read_audit = snapshot
        .capabilities
        .iter()
        .find(|c| c.capability == "read")
        .unwrap();
    assert!(read_audit.requested);
    assert!(read_audit.admitted);
    assert_eq!(read_audit.active_variant.as_deref(), Some("default"));

    let write_audit = snapshot
        .capabilities
        .iter()
        .find(|c| c.capability == "write_file")
        .unwrap();
    assert!(!write_audit.requested);
    assert!(!write_audit.admitted);
}

struct AliasTool {
    name: &'static str,
    aliases: &'static [&'static str],
}

#[async_trait]
impl Tool for AliasTool {
    fn name(&self) -> &str {
        self.name
    }
    fn aliases(&self) -> &'static [&'static str] {
        self.aliases
    }
    fn variant(&self) -> &str {
        "default"
    }
    fn description(&self) -> &str {
        "test tool with aliases"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    async fn call(&self, _arguments: &str) -> Result<String, String> {
        Ok(String::new())
    }
}

#[test]
fn capability_and_pool_admit_aliases() {
    let pool = ToolPool::from_tools(vec![Arc::new(AliasTool {
        name: "custom_tool",
        aliases: &["custom_alias"],
    }) as Arc<dyn Tool>]);

    // 1. Scope naming only the alias ("custom_alias") must admit the canonical tool.
    let alias_agent = ToolSelection::only(["custom_alias"]);
    let resolved = pool.resolve_for(&model(true), &alias_agent, &ToolSelection::unrestricted());
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].name(), "custom_tool");

    let snapshot = pool.snapshot(&alias_agent, &model(true));
    assert_eq!(snapshot.capabilities.len(), 1);
    assert!(snapshot.capabilities[0].requested);
    assert!(snapshot.capabilities[0].admitted);

    // 2. Scope naming the canonical name ("custom_tool") also admits it.
    let canonical_agent = ToolSelection::only(["custom_tool"]);
    let resolved2 = pool.resolve_for(
        &model(true),
        &canonical_agent,
        &ToolSelection::unrestricted(),
    );
    assert_eq!(resolved2.len(), 1);
    assert_eq!(resolved2[0].name(), "custom_tool");

    // 3. Unrelated tool scope rejects it.
    let other_agent = ToolSelection::only(["read"]);
    let resolved3 = pool.resolve_for(&model(true), &other_agent, &ToolSelection::unrestricted());
    assert_eq!(resolved3.len(), 0);
}

#[test]
fn builtin_tool_scope_and_typed_selection() {
    let pool = ToolPool::from_tools(vec![
        Arc::new(AliasTool {
            name: BuiltinTool::ExecuteCommand.as_str(),
            aliases: BuiltinTool::ExecuteCommand.aliases(),
        }) as Arc<dyn Tool>,
        Arc::new(CapTool {
            name: BuiltinTool::Read.as_str(),
            variant: "default",
            requires_vision: false,
        }) as Arc<dyn Tool>,
    ]);

    // Typed BuiltinTool selection
    let agent = ToolSelection::only_builtin([BuiltinTool::ExecuteCommand]);
    let resolved = pool.resolve_for(&model(true), &agent, &ToolSelection::unrestricted());
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].name(), BuiltinTool::ExecuteCommand.as_str());

    // BuiltinTool scope admits canonical name and rejects unknown/unrelated
    assert!(agent.scope.admits_builtin(BuiltinTool::ExecuteCommand));
    assert!(agent.scope.admits("execute_command"));
    assert!(!agent.scope.admits("unknown_tool"));
    assert!(!agent.scope.admits_builtin(BuiltinTool::Read));
}
