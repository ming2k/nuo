//! Subagent profiles: declarative tool-permission roles for autonomous
//! subagents spawned by the `task` tool (and wrappers like
//! `verify_plan_execution`).
//!
//! ## Why this exists
//!
//! Before ADR-0011 the subagent's toolset was a hardcoded filter inside
//! the dispatch tool (`access() == Read` plus a name exclusion for itself).
//! That had two problems:
//!
//! 1. **It was name-driven, not semantic.** `ask_user` is `Read`, so it
//!    passed the filter and reached the subagent. But a subagent is
//!    autonomous and non-interactive — its `UserQuestionRequest` events are
//!    dropped by the subagent tool's event forwarder, so the request deadlocks
//!    until the parent turn is cancelled. The user could see the call but
//!    could not answer it.
//! 2. **The policy was buried in orchestration code.** Adding a second
//!    subagent role (or tightening the existing one) meant editing the
//!    dispatch tool rather than declaring intent.
//!
//! The fix is a profile primitive that expresses the tool policy in terms of
//! [`Tool`] capability axes — [`Tool::scope_target`], [`Tool::requires_user`],
//! [`Tool::spawns_subagent`] — so admission is data-driven and generalizes to
//! future tools without touching the dispatch path.
//!
//! ## The capability axes
//!
//! - [`Tool::scope_target`] — what the call touches (`Read` vs `Write` path). Existing.
//! - [`Tool::requires_user`] — may block on a live human (e.g. `ask_user`).
//! - [`Tool::spawns_subagent`] — dispatches a nested agent (e.g. `task`).
//!
//! Recursion is unconditionally forbidden in any subagent: a tool that
//! `spawns_subagent` is never admitted, regardless of profile. User
//! interaction is a per-profile knob ([`ToolPolicy::allow_user_interaction`])
//! so a future interactive role could opt in once the plumbing surfaces the
//! request; the built-in [`SubAgentProfile::EXPLORE`] profile leaves it off.

use std::sync::Arc;

use crate::model::Model;
use crate::{Tool, ToolScope, ToolSelection, ToolSet};

/// Ceiling on what a subagent may do. There is no capability ladder — a tool is
/// admitted purely by name. [`Tool::spawns_subagent`] and
/// [`Tool::affects_control_flow`] tools are always excluded (recursion and
/// program teardown are absolute, not per-profile toggles). See ADR-0011/0028.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolPolicy {
    /// Which tools a subagent under this policy may use, by name. `None` admits
    /// the full parent toolset (the main agent's shape); `Some(set)` admits only
    /// tools whose `name()` is in the set. This is the sole admission axis —
    /// there is no capability ladder, so adding a new side-effecting tool to the
    /// parent does *not* silently widen a subagent unless its name is listed.
    pub allowed_tools: Option<&'static [&'static str]>,
    /// Whether tools that block on a human ([`Tool::requires_user`]) may run.
    pub allow_user_interaction: bool,
}

impl ToolPolicy {
    /// Returns `true` if a tool may be handed to a subagent under this policy.
    /// Combines the **name scope** ([`allowed_tools`](Self::allowed_tools)) with
    /// the **runtime hard rules** ([`admits_runtime`](Self::admits_runtime)).
    pub fn admits(&self, tool: &dyn Tool) -> bool {
        self.admits_runtime(tool) && self.scope().admits(tool.name())
    }

    /// The subagent hard rules that are independent of the name whitelist:
    /// recursion ([`Tool::spawns_subagent`]) and program teardown
    /// ([`Tool::affects_control_flow`]) are absolute, and human-blocking tools
    /// ([`Tool::requires_user`]) are gated by
    /// [`allow_user_interaction`](Self::allow_user_interaction). These are not
    /// expressible as a capability *name* scope, so the pool resolver (which
    /// handles name scope + the model-capability filter) cannot apply them — the
    /// subagent resolution applies this as a post-filter. See
    /// [`SubAgentProfile::resolve_tools`].
    pub fn admits_runtime(&self, tool: &dyn Tool) -> bool {
        // Recursion is unconditionally forbidden in child sub-agents.
        if tool.spawns_subagent() {
            return false;
        }
        // Control-flow tools (e.g. the abort/exit escape hatch) are
        // unconditionally forbidden in subagents — a spawned agent must never
        // be able to tear down the whole program.
        if tool.affects_control_flow() {
            return false;
        }
        // Tools that block on a human are gated by the profile.
        if tool.requires_user() && !self.allow_user_interaction {
            return false;
        }
        true
    }

    /// This policy's capability **name scope** for the pool resolver: `None`
    /// [`allowed_tools`](Self::allowed_tools) → [`ToolScope::All`]; `Some(set)`
    /// → [`ToolScope::Only`] the listed names. The runtime hard rules
    /// ([`admits_runtime`](Self::admits_runtime)) are layered on separately.
    pub fn scope(&self) -> ToolScope {
        match self.allowed_tools {
            None => ToolScope::All,
            Some(names) => ToolScope::only(names.iter().copied()),
        }
    }
}

/// A declarative subagent role profile: a name, the system-prompt fragment that
/// frames the role, and the [`ToolPolicy`] that scopes what it may touch.
///
/// Symmetric with [`crate::MainAgentProfile`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubAgentProfile {
    pub name: &'static str,
    pub system_prompt: &'static str,
    pub tool_policy: ToolPolicy,
    /// The profile's variant pins (the agent-side **override** axis): a list of
    /// `(capability, variant_id)` this role forces, regardless of the model's
    /// own preference. Empty for every built-in role — they accept whatever
    /// variant the model resolves. A non-empty pin wins over the model's choice
    /// for that capability (agent-over-model), but can still be overridden
    /// *down* by the model's hard capability limit if the pinned variant is
    /// unusable. See [`ToolSet::resolve_for`].
    pub variant_pins: &'static [(&'static str, &'static str)],
    /// Whether a subagent spawned under this profile runs in unattended
    /// execution mode: auto-approves all permissions without human intervention.
    pub unattended: bool,
    /// Whether a subagent spawned under this profile may have the **model**
    /// supply stdin bytes for an `execute_command` call it emits (the opt-in automatic-
    /// flow path). Default `false` for every built-in profile: autonomous
    /// subagents run non-interactively (the L1 hard floor + L2 idle watchdog
    /// keep them from hanging); a profile aimed at automated CI/batch flows
    /// where no human is reachable can set this `true` so the model can feed
    /// a command's stdin directly. Without it, stdin is structurally
    /// unreachable from the model's arguments even inside a subagent.
    pub allow_model_stdin: bool,
}

impl SubAgentProfile {
    /// This profile's [`ToolSelection`] — the agent-identity selector it hands
    /// the pool: the capability **name scope** from its [`ToolPolicy`], plus its
    /// own variant pins (the **override** axis, agent side). Built-in profiles
    /// pin nothing, so they accept the model's variant for every capability;
    /// a profile that pins a variant takes precedence over the model's choice
    /// (agent-over-model) — see [`ToolSet::resolve_for`].
    pub fn selection(&self) -> ToolSelection {
        ToolSelection {
            scope: self.tool_policy.scope(),
            variants: self
                .variant_pins
                .iter()
                .map(|(cap, var)| (cap.to_string(), var.to_string()))
                .collect(),
        }
    }

    /// Resolve the pool down to the toolset a spawned subagent on `model` actually
    /// gets. This is the subagent's whole admission story in one call:
    ///
    /// 1. [`ToolSet::resolve_for`] composes this profile's [`selection`](Self::selection)
    ///    with the model's selection (`model_sel`) — scope by intersection,
    ///    variants by agent-over-model precedence, the model's capability limits
    ///    applied hard.
    /// 2. The subagent **runtime hard rules** ([`ToolPolicy::admits_runtime`]) are
    ///    applied as a post-filter: recursion, control-flow, and (unless the
    ///    profile opts in) human-blocking tools are stripped regardless of name.
    ///
    /// The result is the variant-resolved, model-legal, role-scoped tool list to
    /// hand the child agent.
    pub fn resolve_tools(
        &self,
        toolset: &ToolSet,
        model: &Model,
        model_sel: &ToolSelection,
    ) -> Vec<Arc<dyn Tool>> {
        toolset
            .resolve_for(model, &self.selection(), model_sel)
            .into_iter()
            .filter(|tool| self.tool_policy.admits_runtime(tool.as_ref()))
            .collect()
    }
}

/// Tools a skill-discovery subagent may use: workspace inspection without AST or web dependencies.
pub const SKILL_TOOLS: &[&str] = &["read_text", "find_files", "list_dir", "search_text"];

/// Tools a read-only subagent may use: pure
/// inspection with no side effects. Listed by name so adding a new
/// side-effecting tool to the parent never silently widens these profiles.
pub const READ_ONLY_TOOLS: &[&str] = &[
    "read_text",
    "read_image",
    "find_files",
    "list_dir",
    "search_text",
    "code_query",
    "read_url",
    "search_web",
];

/// Tools a debugging subagent may use: the generic read-only inspection tools
/// plus non-interactive command execution and
/// process inspection tools (`run_command`, `process`) for compiling, reproducing,
/// testing, and running diagnostics.
///
/// Crucially, `edit_text` and `write_file` are strictly excluded from this profile
/// so that a debugging subagent cannot mutate workspace files or attempt code changes;
/// its job is strictly forensic diagnosis, root-cause analysis (RCA), and reporting
/// recommended patches back to the parent developer.
pub const DEBUG_TOOLS: &[&str] = &[
    // Generic read-only inspection.
    "read_text",
    "read_image",
    "find_files",
    "list_dir",
    "search_text",
    "code_query",
    "read_url",
    "search_web",
    // Execution observation for builds, tests, gdb, sanitizers.
    "run_command",
];

impl SubAgentProfile {
    /// Canonical tool lists admitted for autonomous subagents.
    pub const EXPLORE_TOOLS: &'static [&'static str] = READ_ONLY_TOOLS;
    pub const DEBUG_TOOLS: &'static [&'static str] = DEBUG_TOOLS;
    pub const SKILL_TOOLS: &'static [&'static str] = SKILL_TOOLS;
    /// The built-in read-only research role.
    pub const EXPLORE: Self = SubAgentProfile {
        name: "explore",
        system_prompt: "\
You are a delegated research subagent. Your single job is to answer the assigned \
task accurately and concisely. Explore the workspace or the web as needed, \
then write a clear, complete final answer with the key findings (file paths, \
signatures, relevant snippets, conclusions). The toolset handed to you is the \
full set you are permitted to use — work within it, do not request others. \
You are non-interactive: never ask the user any \
question — if information is missing, make a reasonable assumption, note it \
explicitly in your answer, or report that you could not find it. Run at most a \
handful of turns, then answer.",
        tool_policy: ToolPolicy {
            allowed_tools: Some(READ_ONLY_TOOLS),
            allow_user_interaction: false,
        },
        variant_pins: &[],
        unattended: true,
        allow_model_stdin: false,
    };

    /// The session-titling role (ADR-0022). Read-only and non-interactive, its
    /// task is pure text-in/text-out — it admits no tool loop at all.
    pub const TITLE: Self = SubAgentProfile {
        name: "title",
        system_prompt: "\
You are a session-titling subagent. You are shown an excerpt of a conversation \
and asked for a short title that captures what the session is about. Reply with \
only the title — 3 to 7 words, plain text, no quotes, no markdown, no trailing \
punctuation, no preamble. Name the concrete subject of the work (a feature, \
file, bug, or task) rather than a generic word like \"chat\" or \"help\". Write \
the title in the same language as the conversation.",
        tool_policy: ToolPolicy {
            allowed_tools: Some(READ_ONLY_TOOLS),
            allow_user_interaction: false,
        },
        variant_pins: &[],
        unattended: true,
        allow_model_stdin: false,
    };

    /// The debugging subagent role. Unlike `EXPLORE` (pure static read-only),
    /// this subagent is granted non-interactive execution authority (`run_command` and
    /// `process`) so it can reproduce defects, run test suites, execute batch debuggers
    /// (e.g. `gdb -batch`), and capture sanitizer diagnostics (ASan, UBSan, Valgrind)
    /// in an isolated context window.
    pub const DEBUG: Self = SubAgentProfile {
        name: "debug",
        system_prompt: "\
You are a delegated debugging and root-cause analysis subagent. You are handed \
a defect, test failure, crash, compiler error, or anomalous behaviour: your \
mission is to isolate the reproduction, investigate execution traces, and \
identify the root cause without polluting the parent's context with voluminous \
terminal or diagnostic logs. \
The toolset handed to you is the full set you are permitted to use — work \
within it, do not request others. You have inspection tools and non-interactive \
command execution (`run_command`, `process`), but NO file-editing permissions. \
Your role is strictly forensic diagnosis, hypothesis testing, and solution design. \
Key operational guidelines: \
1. Form explicit hypotheses and test them systematically using non-interactive \
   builds, tests, sanitizers (ASan/UBSan/TSan), GDB/LLDB batch mode \
   (e.g., `gdb -batch -ex run -ex \"bt full\"`), or minimal repro commands. \
2. The shell environment is non-interactive: never run commands that block on \
   TTY/stdin or wait for user input. \
3. Keep the parent agent's context clean: do not dump raw multi-megabyte \
   stack traces or verbose build logs into your final reply. Synthesize findings \
   into an actionable, high-signal report. \
4. All your messages come from the parent agent, which cannot see your \
   working context. Your final answer is the complete handoff to the parent \
   developer. Structure your report clearly: \
   - Symptom & Reproduction: Command or conditions to reproduce, plus key error signature. \
   - Root-Cause Analysis (RCA): Exact file, line, and technical explanation of why the failure occurs. \
   - Verification Evidence: How you validated the root cause (e.g. sanitizer frame, reproduction log). \
   - Proposed Fix: The recommended diff or concrete code modifications for the parent developer to apply. \
Run at most a handful of focused turns, then answer.",
        tool_policy: ToolPolicy {
            allowed_tools: Some(DEBUG_TOOLS),
            allow_user_interaction: false,
        },
        variant_pins: &[],
        unattended: true,
        allow_model_stdin: false,
    };

    /// The skill discovery and domain expertise subagent role.
    pub const SKILL: Self = SubAgentProfile {
        name: "skill",
        system_prompt: "\
You are a skill-discovery subagent. Your role is to locate, \
inspect, and synthesize specialized instructions and procedures from available \
skills (project-local `.nuo/skills/`, user-global `~/.local/share/nuo/skills/`, \
and role-scoped skills) to guide the delegated task. Read the \
relevant `SKILL.md` documents and associated reference files, extract the concrete \
rules, tool sequences, edge cases, and best practices, and return an actionable, \
well-structured domain briefing to the calling agent. Do not modify files or ask \
questions of the user; focus strictly on skill discovery and instruction synthesis.",
        tool_policy: ToolPolicy {
            allowed_tools: Some(READ_ONLY_TOOLS),
            allow_user_interaction: false,
        },
        variant_pins: &[],
        unattended: true,
        allow_model_stdin: false,
    };

    /// Static catalog of all built-in subagent profiles.
    pub const ALL: &'static [&'static SubAgentProfile] =
        &[&Self::EXPLORE, &Self::TITLE, &Self::DEBUG, &Self::SKILL];

    /// Find a subagent profile by name.
    pub fn find(name: &str) -> Option<&'static SubAgentProfile> {
        Self::ALL.iter().copied().find(|p| p.name == name)
    }

    /// List all available profile names in the catalog.
    pub fn names() -> Vec<&'static str> {
        Self::ALL.iter().map(|p| p.name).collect()
    }
    /// Filter subagent profiles admitted by an agent delegation policy.
    pub fn admitted_for_delegation(
        delegation: &crate::AgentRoleDelegation,
    ) -> Vec<&'static SubAgentProfile> {
        Self::ALL
            .iter()
            .copied()
            .filter(|p| delegation.admits_subagent(p.name))
            .collect()
    }
}

#[cfg(test)]
mod tests {
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
        assert!(
            SubAgentProfile::EXPLORE
                .tool_policy
                .admits(&make("read_text"))
        );
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
                .admits(&with_user(make("read_text")))
        );
    }

    #[test]
    fn explore_rejects_dispatch_tool_even_if_named_like_a_read() {
        assert!(
            !SubAgentProfile::EXPLORE
                .tool_policy
                .admits(&with_spawn(make("read_text")))
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
        assert!(!permissive.admits(&with_spawn(make("read_text"))));
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
            protocol: crate::WireProtocol::ChatCompletions,
            model_guidance: "",
            effort_levels: &[],
        }
    }

    #[test]
    fn resolve_tools_applies_scope_and_runtime_rules() {
        let toolset = ToolSet::from_tools(vec![
            Arc::new(make("search_text")) as Arc<dyn Tool>,
            Arc::new(make("execute_command")) as Arc<dyn Tool>,
            Arc::new(with_spawn(make("read_text"))) as Arc<dyn Tool>,
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
        assert!(open.admits(&make("read_text")));
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
        assert!(
            SubAgentProfile::DEBUG
                .tool_policy
                .admits(&make("read_text"))
        );
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
                .admits(&with_spawn(make("run_command")))
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
    }
}
