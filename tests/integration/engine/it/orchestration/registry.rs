//! Built-in tool self-registration.

use super::*;

#[test]
fn registry_collects_all_self_registered_tools() {
    let mut builder = ToolFactoryContextBuilder::new();
    builder.provide(Arc::new(SkillRegistry::empty()));
    builder.provide(nuo_engine::AgentIdentity::default());
    let ctx = builder.build();
    let mut collected = collect_toolset(&ctx);
    let sys_ctx = Arc::new(nuo_tool_fs::SystemToolContext::new("."));
    for tool in nuo_tool_fs::create_fs_tools(sys_ctx) {
        collected.upsert(tool);
    }
    let exec_ctx = Arc::new(nuo_tool_exec::ExecToolContext::new("."));
    for tool in nuo_tool_exec::create_exec_tools(exec_ctx) {
        collected.upsert(tool);
    }
    let names: std::collections::HashSet<&str> = collected.capability_names().collect();
    for expected in [
        nuo_tool::BuiltinTool::ExecuteCommand,
        nuo_tool::BuiltinTool::Read,
        nuo_tool::BuiltinTool::WriteFile,
        nuo_tool::BuiltinTool::EditText,
        nuo_tool::BuiltinTool::SearchText,
        nuo_tool::BuiltinTool::FindFiles,
        nuo_tool::BuiltinTool::AskUser,
        nuo_tool::BuiltinTool::ReadUrl,
        nuo_tool::BuiltinTool::SearchWeb,
        nuo_tool::BuiltinTool::RecallMemory,
        nuo_tool::BuiltinTool::CodeQuery,
    ] {
        assert!(
            names.contains(expected.as_str()),
            "tool '{}' missing from collected set",
            expected.as_str()
        );
    }
    // Ghost capabilities are not representable: no factory installs these, so
    // they must not appear under any spelling.
    for ghost in ["use_skill", "list_skills", "read_text", "read_image"] {
        assert!(
            !names.contains(ghost),
            "ghost tool '{ghost}' must not be in the collected set"
        );
    }

    // Role-to-ToolSet integration invariant: developer role must resolve execute_command
    let dev = nuo_protocol::AgentRoleProfile::developer();
    let model = nuo_protocol::resolve_model("test");
    let resolved = collected.resolve_for(
        &model,
        &dev.tools,
        &nuo_protocol::ToolSelection::unrestricted(),
    );
    let resolved_names: std::collections::HashSet<&str> =
        resolved.iter().map(|t| t.name()).collect();
    assert!(
        resolved_names.contains(nuo_tool::BuiltinTool::ExecuteCommand.as_str()),
        "developer role failed to resolve execute_command from collected toolset"
    );
    // ADR-0064: pruned tools must never resolve for developer role
    assert!(!resolved_names.contains(nuo_tool::BuiltinTool::AskUser.as_str()));
    assert!(!resolved_names.contains(nuo_tool::BuiltinTool::Todo.as_str()));

    // Role-to-ToolSet integration invariant: ops role must resolve execute_command and fs tools
    let ops = nuo_protocol::AgentRoleProfile::ops();
    let ops_resolved = collected.resolve_for(
        &model,
        &ops.tools,
        &nuo_protocol::ToolSelection::unrestricted(),
    );
    let ops_resolved_names: std::collections::HashSet<&str> =
        ops_resolved.iter().map(|t| t.name()).collect();
    assert!(
        ops_resolved_names.contains(nuo_tool::BuiltinTool::ExecuteCommand.as_str()),
        "ops role failed to resolve execute_command from collected toolset"
    );
    assert!(
        ops_resolved_names.contains(nuo_tool::BuiltinTool::Read.as_str()),
        "ops role failed to resolve read from collected toolset"
    );
    assert!(
        ops_resolved_names.contains(nuo_tool::BuiltinTool::EditText.as_str()),
        "ops role failed to resolve edit_text from collected toolset"
    );
    assert!(
        ops_resolved_names.contains(nuo_tool::BuiltinTool::WriteFile.as_str()),
        "ops role failed to resolve write_file from collected toolset"
    );
    assert!(
        ops_resolved_names.contains(nuo_tool::BuiltinTool::FindFiles.as_str()),
        "ops role failed to resolve find_files from collected toolset"
    );
    assert!(
        ops_resolved_names.contains(nuo_tool::BuiltinTool::SearchText.as_str()),
        "ops role failed to resolve search_text from collected toolset"
    );
    assert!(
        ops_resolved_names.contains(nuo_tool::BuiltinTool::ReadUrl.as_str()),
        "ops role failed to resolve read_url from collected toolset"
    );
    assert!(
        ops_resolved_names.contains(nuo_tool::BuiltinTool::SearchWeb.as_str()),
        "ops role failed to resolve search_web from collected toolset"
    );
    assert!(!ops_resolved_names.contains(nuo_tool::BuiltinTool::AskUser.as_str()));
    assert!(!ops_resolved_names.contains(nuo_tool::BuiltinTool::Todo.as_str()));

    // Role-to-ToolSet integration invariant: philosophist role must NOT resolve execute_command or fs tools
    let phil = nuo_protocol::AgentRoleProfile::philosophist();
    let phil_resolved = collected.resolve_for(
        &model,
        &phil.tools,
        &nuo_protocol::ToolSelection::unrestricted(),
    );
    let phil_resolved_names: std::collections::HashSet<&str> =
        phil_resolved.iter().map(|t| t.name()).collect();
    assert!(!phil_resolved_names.contains(nuo_tool::BuiltinTool::ExecuteCommand.as_str()));
    assert!(!phil_resolved_names.contains(nuo_tool::BuiltinTool::Read.as_str()));
    assert!(!phil_resolved_names.contains(nuo_tool::BuiltinTool::EditText.as_str()));
    assert!(!phil_resolved_names.contains(nuo_tool::BuiltinTool::WriteFile.as_str()));
    assert!(phil_resolved_names.contains(nuo_tool::BuiltinTool::ReadUrl.as_str()));
    assert!(phil_resolved_names.contains(nuo_tool::BuiltinTool::SearchWeb.as_str()));
}
