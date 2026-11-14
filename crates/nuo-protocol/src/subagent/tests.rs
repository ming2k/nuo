use super::*;
use async_trait::async_trait;

/// A configurable test tool used to exercise every admission branch. The
/// admission axis is now *name* (no capability ladder), so each Stub is
/// parameterized by the tool name it claims.
struct Stub {
    name: &'static str,
    requires_user: bool,
    spawns_subagent: bool,
    affects_control_flow: bool,
}

#[async_trait]
impl Tool for Stub {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "stub"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    fn requires_user(&self) -> bool {
        self.requires_user
    }
    fn spawns_subagent(&self) -> bool {
        self.spawns_subagent
    }
    fn affects_control_flow(&self) -> bool {
        self.affects_control_flow
    }
    async fn call(&self, _arguments: &str) -> Result<String, String> {
        Ok("stub".to_string())
    }
}

/// Build a plain tool named `name`. The flags default to "harmless" — the
/// admission axis is the name itself.
fn make(name: &'static str) -> Stub {
    Stub {
        name,
        requires_user: false,
        spawns_subagent: false,
        affects_control_flow: false,
    }
}

fn with_user(mut t: Stub) -> Stub {
    t.requires_user = true;
    t
}

fn with_spawn(mut t: Stub) -> Stub {
    t.spawns_subagent = true;
    t
}

/// A control-flow tool shape. Used to prove profiles exclude control
/// tools by the control-flow flag, regardless of name.
fn make_control() -> Stub {
    Stub {
        name: "control-stub",
        requires_user: false,
        spawns_subagent: false,
        affects_control_flow: true,
    }
}

#[test]
fn explore_admits_a_whitelisted_read_tool() {
    assert!(SubAgentProfile::EXPLORE.tool_policy.admits(&make("read")));
    assert!(
        SubAgentProfile::EXPLORE
            .tool_policy
            .admits(&make("search_text"))
    );
}

#[test]
fn explore_rejects_a_non_whitelisted_tool() {
    // write_file is not in READ_ONLY_TOOLS — a research explorer must not
    // mutate files.
    assert!(
        !SubAgentProfile::EXPLORE
            .tool_policy
            .admits(&make("write_file"))
    );
    // Command execution is also not whitelisted.
    assert!(
        !SubAgentProfile::EXPLORE
            .tool_policy
            .admits(&make("execute_command"))
    );
}

#[test]
fn explore_rejects_a_whitelisted_tool_that_requires_user() {
    assert!(
        !SubAgentProfile::EXPLORE
            .tool_policy
            .admits(&with_user(make("read")))
    );
}

#[test]
fn explore_rejects_dispatch_tool_even_if_named_like_a_read() {
    assert!(
        !SubAgentProfile::EXPLORE
            .tool_policy
            .admits(&with_spawn(make("read")))
    );
}

#[test]
fn explore_rejects_control_flow_tool() {
    assert!(!SubAgentProfile::EXPLORE.tool_policy.admits(&make_control()));
}

#[test]
fn recursion_is_rejected_even_by_a_permissive_policy() {
    let permissive = ToolPolicy {
        allowed_tools: None,
        allow_user_interaction: true,
    };
    assert!(!permissive.admits(&with_spawn(make("read"))));
    assert!(permissive.admits(&make("execute_command")));
}

#[test]
fn control_flow_is_rejected_even_by_a_permissive_policy() {
    let permissive = ToolPolicy {
        allowed_tools: None,
        allow_user_interaction: true,
    };
    assert!(!permissive.admits(&make_control()));
}

fn test_model() -> Model {
    Model {
        id: "test",
        family: "test",
        context_window: 100_000,
        thinking: crate::reasoning::ReasoningSupport::None,
        tool_call: true,
        vision: true,
        protocol: crate::WireProtocol::OpenAiChatCompletions,
        effort_levels: &[],
        default_effort: Some(crate::Effort::High),
        wire_entities: None,
    }
}

#[test]
fn resolve_tools_applies_scope_and_runtime_rules() {
    let toolset = ToolSet::from_tools(vec![
        Arc::new(make("search_text")) as Arc<dyn Tool>,
        Arc::new(make("execute_command")) as Arc<dyn Tool>,
        Arc::new(with_spawn(make("read"))) as Arc<dyn Tool>,
    ]);
    let selected = SubAgentProfile::EXPLORE.resolve_tools(
        &toolset,
        &test_model(),
        &ToolSelection::unrestricted(),
    );
    let names: Vec<&str> = selected.iter().map(|t| t.name()).collect();
    assert_eq!(names, vec!["search_text"]);
}

#[test]
fn none_allowed_tools_admits_everything_named() {
    let open = ToolPolicy {
        allowed_tools: None,
        allow_user_interaction: false,
    };
    assert!(open.admits(&make("read")));
    assert!(open.admits(&make("execute_command")));
    assert!(open.admits(&make("write_file")));
}

#[test]
fn explore_profile_excludes_unlisted_tools() {
    assert!(
        !SubAgentProfile::EXPLORE
            .tool_policy
            .admits(&make("market_data"))
    );
    assert!(
        !SubAgentProfile::EXPLORE
            .tool_policy
            .admits(&make("backtest"))
    );
    assert!(
        !SubAgentProfile::EXPLORE
            .tool_policy
            .admits(&make("place_order"))
    );
    assert!(
        !SubAgentProfile::EXPLORE
            .tool_policy
            .admits(&make("cancel_order"))
    );
    assert!(
        !SubAgentProfile::EXPLORE
            .tool_policy
            .admits(&make("list_positions"))
    );
}

#[test]
fn debug_profile_admits_read_and_command_but_excludes_writes_and_recursion() {
    assert!(SubAgentProfile::DEBUG.tool_policy.admits(&make("read")));
    assert!(
        SubAgentProfile::DEBUG
            .tool_policy
            .admits(&make("search_text"))
    );
    assert!(
        SubAgentProfile::DEBUG
            .tool_policy
            .admits(&make("code_query"))
    );
    assert!(
        SubAgentProfile::DEBUG
            .tool_policy
            .admits(&make("execute_command"))
    );
    assert!(
        !SubAgentProfile::DEBUG
            .tool_policy
            .admits(&make("run_command"))
    );

    assert!(
        !SubAgentProfile::DEBUG
            .tool_policy
            .admits(&make("edit_text"))
    );
    assert!(
        !SubAgentProfile::DEBUG
            .tool_policy
            .admits(&make("write_file"))
    );
    assert!(!SubAgentProfile::DEBUG.tool_policy.admits(&make("todo")));

    assert!(
        !SubAgentProfile::DEBUG
            .tool_policy
            .admits(&make("some_new_tool"))
    );
    assert!(
        !SubAgentProfile::DEBUG
            .tool_policy
            .admits(&make("market_data"))
    );

    assert!(
        !SubAgentProfile::DEBUG
            .tool_policy
            .admits(&with_spawn(make("execute_command")))
    );
    assert!(!SubAgentProfile::DEBUG.tool_policy.admits(&make_control()));

    assert!(
        !SubAgentProfile::DEBUG
            .tool_policy
            .admits(&with_user(make("ask_user")))
    );
}

#[allow(clippy::assertions_on_constants)]
#[test]
fn debug_profile_runs_unattended() {
    assert!(SubAgentProfile::DEBUG.unattended);
    assert!(!SubAgentProfile::DEBUG.allow_model_stdin);
}

#[test]
fn subagent_profile_catalog_and_filtering() {
    assert_eq!(SubAgentProfile::ALL.len(), 4);
    assert_eq!(
        SubAgentProfile::find("explore").map(|p| p.name),
        Some("explore")
    );
    assert_eq!(
        SubAgentProfile::find("debug").map(|p| p.name),
        Some("debug")
    );
    assert_eq!(
        SubAgentProfile::find("skill").map(|p| p.name),
        Some("skill")
    );
    assert_eq!(SubAgentProfile::find("nonexistent"), None);

    let dev_delegation = crate::AgentRoleProfile::DEVELOPER;
    let dev_subagents = SubAgentProfile::admitted_for_delegation(&dev_delegation);
    assert_eq!(dev_subagents.len(), 2);
    let dev_names: Vec<&str> = dev_subagents.iter().map(|p| p.name).collect();
    assert_eq!(dev_names, vec!["explore", "debug"]);

    let phil_delegation = crate::AgentRoleProfile::PHILOSOPHIST;
    let phil_subagents = SubAgentProfile::admitted_for_delegation(&phil_delegation);
    assert_eq!(phil_subagents.len(), 1);
    let names: Vec<&str> = phil_subagents.iter().map(|p| p.name).collect();
    assert_eq!(names, vec!["explore"]);

    let ops_delegation = crate::AgentRoleProfile::OPS;
    let ops_subagents = SubAgentProfile::admitted_for_delegation(&ops_delegation);
    assert_eq!(ops_subagents.len(), 4);
    let names: Vec<&str> = ops_subagents.iter().map(|p| p.name).collect();
    assert_eq!(names, vec!["explore", "title", "debug", "skill"]);
}
