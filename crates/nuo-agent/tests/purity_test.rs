//! Purity and collaboration-surface invariants.
//!
//! These assert the architectural boundary, not a feature: an agent's exposed
//! capability must never depend on whether it happens to be attached to a
//! transport, and every tool advertised to the model must be executable by it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_agent::agent::Agent;
use nuo_agent::collaboration::CollaborationTool;
use nuo_agent::provider::MockProvider;
use acp::{AgentAddress, AgentManifest, Fabric};

fn manifest(uri: &str, name: &str, description: &str) -> AgentManifest {
    AgentManifest::new(AgentAddress::parse(uri).unwrap(), name, description)
}

fn collaboration_names() -> Vec<String> {
    let mut names: Vec<String> = CollaborationTool::ALL
        .iter()
        .map(|tool| tool.name().to_string())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn standalone_agent_has_zero_protocol_surface() {
    let agent = Agent::builder("agent://local/solo")
        .name("Solo Agent")
        .description("Works alone")
        .provider(MockProvider::new())
        .build()
        .await
        .unwrap();

    // No collaboration binding...
    assert!(agent.collaboration().is_none());

    // ...and no collaboration tool is reachable by the model.
    let names = agent.tool_names();
    for tool in collaboration_names() {
        assert!(
            !names.contains(&tool),
            "standalone agent must not expose `{tool}`, found: {names:?}"
        );
    }
    assert!(
        names.is_empty(),
        "standalone agent should expose no tools at all"
    );

    let specs = agent.model_specs();
    assert!(
        specs.is_empty(),
        "standalone agent must advertise no tools to the provider"
    );
}

#[tokio::test]
async fn room_member_exposes_exactly_the_collaboration_tools() {
    let room = Fabric::new("squad");
    let _peer_mailbox = room
        .join(
            manifest("agent://local/peer", "Peer", "Does peer things"),
            16,
        )
        .await;

    let agent = Agent::builder("agent://local/dev")
        .name("Dev Agent")
        .description("Writes code")
        .provider(MockProvider::new())
        .connect_to(&room)
        .build()
        .await
        .unwrap();

    assert!(agent.collaboration().is_some());

    let mut expected = collaboration_names();
    expected.sort();
    assert_eq!(agent.tool_names(), expected);

    // Advertisement parity: exactly the advertised tools are callable.
    assert_eq!(agent.advertised_tool_names(), expected);
}

#[tokio::test]
async fn tool_schemas_match_registered_tools() {
    let room = Fabric::new("squad");
    let _peer = room
        .join(manifest("agent://local/peer", "Peer", "Peer work"), 16)
        .await;

    let agent = Agent::builder("agent://local/dev")
        .name("Dev")
        .description("Dev work")
        .provider(MockProvider::new())
        .connect_to(&room)
        .build()
        .await
        .unwrap();

    let specs = agent.model_specs();
    assert_eq!(specs.len(), CollaborationTool::ALL.len());

    for spec in &specs {
        let name = spec["function"]["name"].as_str().unwrap();
        assert!(
            collaboration_names().contains(&name.to_string()),
            "unexpected advertised tool `{name}`"
        );

        // Every advertised tool must have a well-formed, closed schema so the
        // provider can validate arguments and reject unknown fields.
        let params = &spec["function"]["parameters"];
        assert_eq!(
            params["type"], "object",
            "tool `{name}` needs an object schema"
        );
        assert!(
            params["properties"].is_object(),
            "tool `{name}` must declare properties"
        );
        assert_eq!(
            params["additionalProperties"], false,
            "tool `{name}` must forbid unknown arguments"
        );
    }
}

#[tokio::test]
async fn peer_directory_never_leaks_into_a_lone_agents_prompt() {
    // A room with only this agent: the prompt must not contain a peer section.
    let room = Fabric::new("empty-squad");
    let agent = Agent::builder("agent://local/solo")
        .name("Solo")
        .description("Alone")
        .provider(MockProvider::new())
        .connect_to(&room)
        .build()
        .await
        .unwrap();

    let prompt = agent.system_prompt().await;
    assert!(!prompt.contains("Other agents you can delegate to"));
    assert!(!prompt.contains("delegate_to_peer"));
}

#[tokio::test]
async fn peer_directory_appears_once_a_peer_joins() {
    let room = Fabric::new("squad");
    let agent = Agent::builder("agent://local/dev")
        .name("Dev Agent")
        .description("Writes code")
        .provider(MockProvider::new())
        .connect_to(&room)
        .build()
        .await
        .unwrap();

    assert!(!agent.system_prompt().await.contains("Kanban Agent"));

    // A peer joining later is visible without rebuilding the agent.
    let _kanban = room
        .join(
            manifest(
                "agent://local/kanban",
                "Kanban Agent",
                "Tracks issues in Linear",
            ),
            16,
        )
        .await;

    let prompt = agent.system_prompt().await;
    assert!(prompt.contains("Other agents you can delegate to"));
    assert!(prompt.contains("Kanban Agent"));
    assert!(prompt.contains("Tracks issues in Linear"));

    // The agent's own card must not appear as a delegation candidate. Scope the
    // check to the directory section, since the self-description line legitimately
    // contains the agent's own name and description.
    let directory = prompt
        .split("Other agents you can delegate to:")
        .nth(1)
        .expect("peer directory section should be present");
    assert!(
        !directory.contains("agent://local/dev"),
        "own address must not be a delegation target: {directory}"
    );
}
