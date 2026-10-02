//! Comprehensive verification of full-system capabilities:
//! 1. Scope-isolated Long-term Memory (Working vs Long-term, Scope privacy).
//! 2. Autonomous Task Claiming via Subscription Filters (No-mention auto-routing).
//! 3. Anti-storm Channel Circuit Breaker.
//! 4. Human-in-the-loop Tool Approval (ApprovalPolicy / ApprovalHandler).
//! 5. Distributed Channel Event Replication across multi-host rooms.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_agent::agent::Agent;
use nuo_agent::memory::{InMemoryMemory, Memory, MemoryQuery, MemoryScope};
use nuo_agent::provider::MockProvider;
use nuo_agent::session::SessionKey;
use nuo_agent::tools::{
    ApprovalDecision, ApprovalHandler, DynamicTool, ToolCallRequest, ToolContext,
};
use acp::transport::in_memory_pair;
use acp::{AgentAddress, ChannelId, Fabric, SubscriptionFilter, SubscriptionMode};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use uuid::Uuid;

fn addr(uri: &str) -> AgentAddress {
    AgentAddress::parse(uri).unwrap()
}

// =========================================================================
// 1. Long-term Memory Isolation & Recall Tests
// =========================================================================

#[tokio::test]
async fn memory_respects_channel_and_peer_privacy_boundaries() {
    let memory = Arc::new(InMemoryMemory::new());

    let channel_ops = ChannelId::new("ops").unwrap();
    let channel_random = ChannelId::new("random").unwrap();
    let peer_alice = addr("agent://local/alice");
    let peer_bob = addr("agent://local/bob");

    // Seed facts with different scopes
    memory
        .add_fact(
            "Ops standard: all database migrations run at 2am",
            MemoryScope::Channel(channel_ops.clone()),
        )
        .await;

    memory
        .add_fact(
            "Alice secret: prefers Rust over Go",
            MemoryScope::Peer(peer_alice.clone()),
        )
        .await;

    memory
        .add_fact(
            "Company policy: remote-first workforce",
            MemoryScope::Public,
        )
        .await;

    // 1. Query from Ops channel conversation
    let ops_query = MemoryQuery::new("database", SessionKey::for_channel(channel_ops.clone()));
    let ops_results = memory.recall(&ops_query).await.unwrap();
    let ops_contents: Vec<&str> = ops_results.iter().map(|f| f.content.as_str()).collect();

    assert!(
        ops_contents
            .iter()
            .any(|c| c.contains("database migrations"))
    );
    assert!(
        !ops_contents.iter().any(|c| c.contains("Alice secret")),
        "peer secret must not leak to channel!"
    );

    // 2. Query from Alice 1:1 conversation
    let alice_query = MemoryQuery::new("prefers", SessionKey::for_peer(&peer_alice));
    let alice_results = memory.recall(&alice_query).await.unwrap();
    let alice_contents: Vec<&str> = alice_results.iter().map(|f| f.content.as_str()).collect();

    assert!(alice_contents.iter().any(|c| c.contains("Alice secret")));
    assert!(
        !alice_contents
            .iter()
            .any(|c| c.contains("database migrations")),
        "channel traffic must not leak to private 1:1!"
    );

    // 3. Query from Bob 1:1 conversation (must NOT see Alice's secret)
    let bob_query = MemoryQuery::new("prefers", SessionKey::for_peer(&peer_bob));
    let bob_results = memory.recall(&bob_query).await.unwrap();
    assert!(
        bob_results.is_empty(),
        "Bob must not see Alice's private memory!"
    );

    // 4. Query from Random channel (must NOT see Ops channel's fact)
    let random_query = MemoryQuery::new("database", SessionKey::for_channel(channel_random));
    let random_results = memory.recall(&random_query).await.unwrap();
    assert!(
        random_results.is_empty(),
        "Random channel must not see Ops facts!"
    );
}

#[tokio::test]
async fn memory_is_automatically_injected_into_prompt() {
    let memory = Arc::new(InMemoryMemory::new());
    memory
        .add_fact(
            "Deployment guideline: always tag PRs with vX.Y.Z",
            MemoryScope::Public,
        )
        .await;

    let agent = Agent::builder("agent://local/assistant")
        .name("Assistant")
        .provider(MockProvider::new())
        .with_memory_arc(memory)
        .build()
        .await
        .unwrap();

    let prompt = agent.system_prompt().await;
    assert!(prompt.contains("Relevant long-term memories"));
    assert!(prompt.contains("Deployment guideline"));
}

// =========================================================================
// 2. Autonomous Task Claiming via Subscription Filters
// =========================================================================

#[tokio::test]
async fn unaddressed_message_wakes_only_filtered_agent() {
    let room = Fabric::new("squad");

    let dev_addr = addr("agent://local/dev");
    let dba_addr = addr("agent://local/dba");
    let user_addr = addr("agent://local/user");

    // Dev Agent
    let dev = Agent::builder(dev_addr.as_str())
        .name("Dev")
        .provider(MockProvider::new())
        .connect_to(&room)
        .build()
        .await
        .unwrap();

    // DBA Agent
    let dba = Agent::builder(dba_addr.as_str())
        .name("DBA")
        .provider(MockProvider::new())
        .connect_to(&room)
        .build()
        .await
        .unwrap();

    // User / Poster
    let _user = Agent::builder(user_addr.as_str())
        .name("User")
        .provider(MockProvider::new())
        .connect_to(&room)
        .build()
        .await
        .unwrap();

    let channel_id = ChannelId::new("engineering").unwrap();
    room.open_channel(channel_id.clone()).await;

    // Dev listens for code/rust/frontend
    room.subscribe(
        dev.address(),
        &channel_id,
        SubscriptionMode::Filtered(SubscriptionFilter::new(vec!["code", "rust", "frontend"])),
    )
    .await
    .unwrap();

    // DBA listens for sql/database/postgres
    room.subscribe(
        dba.address(),
        &channel_id,
        SubscriptionMode::Filtered(SubscriptionFilter::new(vec!["sql", "database", "deadlock"])),
    )
    .await
    .unwrap();

    // 1. Post unaddressed message about database: should wake ONLY DBA
    let (_, notified) = room
        .publish(
            &user_addr,
            &channel_id,
            "We have a severe database deadlock in production",
            Vec::new(),
        )
        .await
        .unwrap();

    assert_eq!(
        notified,
        vec![dba_addr.clone()],
        "Only DBA should be woken for database issue"
    );

    // 2. Post unaddressed message about rust: should wake ONLY Dev
    let (_, notified) = room
        .publish(
            &user_addr,
            &channel_id,
            "The rust compiler crashed on CI",
            Vec::new(),
        )
        .await
        .unwrap();

    assert_eq!(
        notified,
        vec![dev_addr.clone()],
        "Only Dev should be woken for rust issue"
    );

    // 3. Post unaddressed message about marketing: wakes NOBODY
    let (_, notified) = room
        .publish(
            &user_addr,
            &channel_id,
            "New marketing flyer is ready for review",
            Vec::new(),
        )
        .await
        .unwrap();

    assert!(
        notified.is_empty(),
        "Nobody should be woken for unrelated chatter"
    );
}

// =========================================================================
// 3. Anti-storm Channel Circuit Breaker Tests
// =========================================================================

#[tokio::test]
async fn channel_circuit_breaker_trips_and_suppresses_automated_storms() {
    let room = Fabric::new("squad");
    let bot_a = addr("agent://local/bot_a");
    let bot_b = addr("agent://local/bot_b");

    let _a = Agent::builder(bot_a.as_str())
        .name("Bot A")
        .provider(MockProvider::new())
        .connect_to(&room)
        .build()
        .await
        .unwrap();

    let _b = Agent::builder(bot_b.as_str())
        .name("Bot B")
        .provider(MockProvider::new())
        .connect_to(&room)
        .build()
        .await
        .unwrap();

    let channel_id = ChannelId::new("ping-pong").unwrap();
    let channel = room.open_channel(channel_id.clone()).await;

    room.subscribe(&bot_a, &channel_id, SubscriptionMode::All)
        .await
        .unwrap();
    room.subscribe(&bot_b, &channel_id, SubscriptionMode::All)
        .await
        .unwrap();

    // Set circuit breaker threshold to 3 consecutive turns
    channel.set_circuit_breaker(Some(3)).await;

    // Turn 1
    let (_, notified) = room
        .publish(&bot_a, &channel_id, "Msg 1", Vec::new())
        .await
        .unwrap();
    assert!(!notified.is_empty());
    assert!(!channel.is_circuit_breaker_tripped().await);

    // Turn 2
    let (_, notified) = room
        .publish(&bot_b, &channel_id, "Msg 2", Vec::new())
        .await
        .unwrap();
    assert!(!notified.is_empty());
    assert!(!channel.is_circuit_breaker_tripped().await);

    // Turn 3: 3rd turn still delivers
    let (_, notified) = room
        .publish(&bot_a, &channel_id, "Msg 3", Vec::new())
        .await
        .unwrap();
    assert!(!notified.is_empty());
    assert!(!channel.is_circuit_breaker_tripped().await);

    // Turn 4: exceeds threshold of 3 -> circuit breaker trips and suppresses!
    let (_, notified) = room
        .publish(&bot_b, &channel_id, "Msg 4", Vec::new())
        .await
        .unwrap();
    assert!(
        notified.is_empty(),
        "Notifications must be suppressed when breaker trips!"
    );
    assert!(channel.is_circuit_breaker_tripped().await);

    // Human/admin resets breaker
    channel.reset_circuit_breaker().await;
    assert!(!channel.is_circuit_breaker_tripped().await);

    // Turn 5: automated notifications resume!
    let (_, notified) = room
        .publish(&bot_a, &channel_id, "Msg 5", Vec::new())
        .await
        .unwrap();
    assert!(
        !notified.is_empty(),
        "Notifications should resume after reset"
    );
}

// =========================================================================
// 4. Human-in-the-Loop Tool Approval Tests
// =========================================================================

struct MockApprovalHandler {
    allow_deletion: AtomicBool,
}

#[async_trait::async_trait]
impl ApprovalHandler for MockApprovalHandler {
    async fn request_approval(
        &self,
        request: &ToolCallRequest,
        _ctx: &ToolContext,
    ) -> nuo_tool::Result<ApprovalDecision> {
        if request.tool_name == "dangerous_wipe_db" {
            if self.allow_deletion.load(Ordering::SeqCst) {
                Ok(ApprovalDecision::Approved)
            } else {
                Ok(ApprovalDecision::reject(
                    "Admin denied database wipe request",
                ))
            }
        } else {
            Ok(ApprovalDecision::Approved)
        }
    }
}

#[tokio::test]
async fn tool_approval_blocks_unauthorized_execution_and_reports_observation() {
    let provider = Arc::new(MockProvider::new());
    let approval_handler = Arc::new(MockApprovalHandler {
        allow_deletion: AtomicBool::new(false),
    });

    let dangerous_tool = DynamicTool::new(
        "dangerous_wipe_db",
        "Wipes the entire production database",
        json!({
            "type": "object",
            "properties": {"target": {"type": "string"}},
            "required": ["target"]
        }),
        |_args| async move { Ok("DATABASE WIPED".to_string()) },
    )
    .with_approval_check(|_args| true); // Requires approval!

    let agent = Agent::builder("agent://local/safe_worker")
        .name("SafeWorker")
        .provider_arc(provider.clone())
        .tool(dangerous_tool)
        .with_approval_handler_arc(approval_handler.clone())
        .build()
        .await
        .unwrap();

    // Round 1: Model attempts to wipe database
    provider
        .push_response(nuo_agent::provider::ModelResponse::tool_call(
            "call_1",
            "dangerous_wipe_db",
            json!({"target": "prod"}),
            10,
            10,
        ))
        .await;

    // Round 2: Model sees rejection and gracefully answers
    provider
        .push_text("The wipe operation was rejected by admin policy.")
        .await;

    let answer = agent.prompt("Please wipe the prod database").await.unwrap();
    assert_eq!(answer, "The wipe operation was rejected by admin policy.");

    // Now enable approval and run again
    approval_handler
        .allow_deletion
        .store(true, Ordering::SeqCst);
    provider
        .push_response(nuo_agent::provider::ModelResponse::tool_call(
            "call_2",
            "dangerous_wipe_db",
            json!({"target": "prod"}),
            10,
            10,
        ))
        .await;
    provider.push_text("Database successfully wiped.").await;

    let answer2 = agent
        .prompt("Please wipe the prod database now")
        .await
        .unwrap();
    assert_eq!(answer2, "Database successfully wiped.");
}

// =========================================================================
// 5. Distributed Channel Event Replication Seam
// =========================================================================

#[tokio::test]
async fn channel_replication_syncs_messages_across_remote_rooms() {
    // Host 1: Room "us-east"
    let room_east = Fabric::new("us-east");
    // Host 2: Room "eu-west"
    let room_west = Fabric::new("eu-west");

    let (east_link, mut west_link) = in_memory_pair(32);

    // Hook east outbound replicator to east_link's sender
    room_east.add_channel_replicator(east_link.sender).await;

    let channel_id = ChannelId::new("global-announcements").unwrap();
    let channel_east = room_east.open_channel(channel_id.clone()).await;
    let channel_west = room_west.open_channel(channel_id.clone()).await;

    let author = addr("agent://us-east/broadcaster");
    let _broadcaster = Agent::builder(author.as_str())
        .name("Broadcaster")
        .provider(MockProvider::new())
        .connect_to(&room_east)
        .build()
        .await
        .unwrap();

    // Spawn a bridge worker forwarding envelopes from east to west
    let room_west_clone = room_west.clone();
    tokio::spawn(async move {
        while let Ok(envelope) = west_link.receiver.recv().await {
            if let acp::MessageIntent::ChannelSync(payload) = envelope.intent {
                let _ = room_west_clone.ingest_channel_sync(&payload).await;
            }
        }
    });

    // Publish on US East
    let (published_msg, _) = room_east
        .publish(
            &author,
            &channel_id,
            "Global maintenance scheduled for midnight UTC",
            Vec::new(),
        )
        .await
        .unwrap();

    assert_eq!(published_msg.seq, 1);
    assert_eq!(channel_east.len().await, 1);

    // Wait a brief tick for the cross-host bridge task to ingest
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Assert that the message has been replicated into EU West's channel log with exact seq and content!
    let west_messages = channel_west.recent(10).await;
    assert_eq!(
        west_messages.len(),
        1,
        "Message should be replicated to EU West!"
    );
    assert_eq!(west_messages[0].seq, 1);
    assert_eq!(
        west_messages[0].body,
        "Global maintenance scheduled for midnight UTC"
    );
    assert_eq!(west_messages[0].from, author);
}

// =========================================================================
// 6. Claim-Check Invoice, ObservationStore, and InspectTool Tests
// =========================================================================

#[tokio::test]
async fn file_observation_store_persists_and_slices_on_disk() {
    use nuo_agent::token::{FileObservationStore, ObservationStore};

    let temp_store = FileObservationStore::in_temp_dir().unwrap();
    let handle = "call:test_file_call";
    let text = "Line 1\nLine 2: Target secret line\nLine 3\nLine 4\n";

    temp_store.store(handle, text.to_string()).await.unwrap();

    assert!(temp_store.contains(handle).await);
    assert_eq!(temp_store.get_length(handle).await, Some(text.len()));

    // Slice characters
    let slice = temp_store.fetch_slice(handle, 7, 26).await.unwrap();
    assert_eq!(slice, "Line 2: Target secret line");
}

#[tokio::test]
async fn inspect_tool_rehydrates_invoice_slice() {
    use nuo_agent::token::{InMemoryObservationStore, ObservationStore};
    use nuo_agent::tools::{InspectTool, Tool};

    let obs_store = Arc::new(InMemoryObservationStore::new());
    let handle = "call:compile_errors";
    let raw_logs =
        "Compiler error on line 42: mismatched types\nCompiler error on line 88: unreachable code";

    obs_store.store(handle, raw_logs.to_string()).await.unwrap();

    let inspect_tool = InspectTool::new(obs_store);
    assert_eq!(inspect_tool.name(), "inspect");

    let args = json!({
        "target": handle,
        "offset": 0,
        "limit": 43,
    });

    let result = inspect_tool.execute_simple(args).await.unwrap();
    assert!(result.contains("call:compile_errors"));
    assert!(result.contains("Compiler error on line 42: mismatched types"));
}

#[tokio::test]
async fn agent_with_observation_store_offloads_large_tool_output_and_allows_inspection() {
    use nuo_agent::provider::ModelResponse;
    use nuo_agent::token::{
        CompactionPolicy, InMemoryObservationStore, ObservationStore, OffloadMode,
    };

    let provider = Arc::new(MockProvider::new());
    let obs_store = Arc::new(InMemoryObservationStore::new());

    let policy = CompactionPolicy {
        auto_offload: Some(OffloadMode::Partial),
        max_tool_output_chars: 150,
        ..Default::default()
    };

    // Tool that emits large data
    let dump_tool = DynamicTool::new(
        "dump_data",
        "Dumps large output",
        json!({"type": "object"}),
        |_args| async move { Ok("X".repeat(1000)) },
    );

    let agent = Agent::builder("agent://local/offloader")
        .name("Offloader")
        .provider_arc(provider.clone())
        .tool(dump_tool)
        .budget(nuo_agent::token::TokenBudget::new(1_000, 10))
        .compaction_policy(policy)
        .with_observation_store_arc(obs_store.clone())
        .build()
        .await
        .unwrap();

    // The inspect tool should have been automatically registered!
    assert!(agent.tool_names().contains(&"inspect".to_string()));

    // Round 1: Model calls dump_data
    provider
        .push_response(ModelResponse::tool_call(
            "call_dump_99",
            "dump_data",
            json!({}),
            10,
            10,
        ))
        .await;

    // Round 2: Model sees invoice and calls inspect
    provider
        .push_response(ModelResponse::tool_call(
            "call_inspect_1",
            "inspect",
            json!({"target": "call:call_dump_99", "offset": 0, "limit": 20}),
            10,
            10,
        ))
        .await;

    // Round 3: Model completes
    provider
        .push_text("Inspected raw data successfully: XXXXXXXXXXXXXXXXXXXX")
        .await;

    let answer = agent.prompt("Get data and inspect it").await.unwrap();
    assert!(answer.contains("Inspected raw data successfully"));

    // Verify raw output was saved in observation store under "call:call_dump_99"
    assert!(obs_store.contains("call:call_dump_99").await);
    let slice = obs_store
        .fetch_slice("call:call_dump_99", 0, 5)
        .await
        .unwrap();
    assert_eq!(slice, "XXXXX");
}

// =========================================================================
// 7. Cross-Session Autobiographical Memory, CommandTool & P2P Delegation
// =========================================================================

#[tokio::test]
async fn cross_session_awareness_recalls_past_actions_from_other_surfaces() {
    let memory = Arc::new(InMemoryMemory::new());
    let provider = Arc::new(MockProvider::new());

    let agent = Agent::builder("agent://local/worker")
        .name("Worker")
        .provider_arc(provider.clone())
        .with_memory_arc(memory.clone())
        .build()
        .await
        .unwrap();

    // Surface 1: In a Channel (Session A), agent fixes the auth crash
    let channel_key = SessionKey::for_channel(ChannelId::new("dev-delivery").unwrap());
    provider
        .push_text("Fixed null pointer crash in AuthMiddleware and pushed commit 7a8b")
        .await;

    let res_a = agent
        .run_turn(
            &channel_key,
            Uuid::new_v4(),
            "Fix the login null pointer crash",
        )
        .await
        .unwrap();
    assert!(res_a.contains("Fixed null pointer crash in AuthMiddleware"));

    // Yield brief tick for background memory observation task
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Surface 2: In a 1:1 conversation with Alice (Session B)
    let alice_key = SessionKey::for_peer(&addr("agent://local/alice"));
    let alice_prompt = agent.system_prompt_for(&alice_key).await;

    // The agent's prompt in Alice's session must contain its self-action from the channel!
    assert!(
        alice_prompt.contains("[Self-Action in channel:dev-delivery]"),
        "Agent should perceive what it did in the channel session!"
    );
    assert!(alice_prompt.contains("Fixed null pointer crash in AuthMiddleware"));
}

#[tokio::test]
async fn command_tool_executes_safely_and_triggers_approval_on_danger() {
    use nuo_agent::tools::{CommandTool, Tool};

    let cmd_tool = CommandTool::new();

    let ctx = ToolContext::default();
    // 1. Safe command execution
    let safe_args = json!({
        "command": "echo 'Hello from CommandTool'"
    });
    assert!(!cmd_tool.requires_approval(&ctx, &safe_args));

    let output = cmd_tool.execute_simple(safe_args).await.unwrap();
    assert!(output.contains("Hello from CommandTool"));

    // 2. High risk command triggers approval requirement
    let dangerous_args = json!({
        "command": "rm -rf /tmp/test_dir"
    });
    assert!(
        cmd_tool.requires_approval(&ctx, &dangerous_args),
        "High risk command must require approval!"
    );
}

#[tokio::test]
async fn direct_p2p_delegation_without_room_exposes_only_two_tools() {
    use nuo_agent::provider::ModelResponse;
    use acp::{AgentManifest, Fabric};

    let fabric = Fabric::new("p2p-mesh");

    let kanban_addr = addr("agent://local/kanban");
    let dev_addr = addr("agent://local/dev");

    let _kanban_manifest = AgentManifest::new(
        kanban_addr.clone(),
        "Kanban",
        "Tracks issues and creates tickets",
    );

    let dev_provider = Arc::new(MockProvider::new());
    let kanban_provider = Arc::new(MockProvider::new());

    // Kanban agent connected to fabric
    let kanban = Agent::builder(kanban_addr.as_str())
        .name("Kanban")
        .provider_arc(kanban_provider.clone())
        .connect_to(&fabric)
        .build()
        .await
        .unwrap();

    // Dev agent connected to fabric with p2p delegation surface (NO CHANNEL TOOLS!)
    let dev = Agent::builder(dev_addr.as_str())
        .name("Dev")
        .provider_arc(dev_provider.clone())
        .connect_to(&fabric)
        .with_p2p_delegation()
        .build()
        .await
        .unwrap();

    // Assert: Dev agent has ONLY the 2 direct delegation tools (NO channel tools!)
    let tool_names = dev.tool_names();
    assert_eq!(tool_names, vec!["delegate_to_peer", "list_peers"]);
    assert!(!tool_names.contains(&"open_channel".to_string()));
    assert!(!tool_names.contains(&"publish_to_channel".to_string()));

    // Serve loops
    let (kanban, kanban_inbox) = kanban.into_serving().unwrap();
    tokio::spawn(async move {
        let _ = kanban.serve(kanban_inbox).await;
    });

    let (dev, dev_inbox) = dev.into_serving().unwrap();
    let dev_serving = dev.clone();
    tokio::spawn(async move {
        let _ = dev_serving.serve(dev_inbox).await;
    });

    // Dev delegates to Kanban 1:1
    dev_provider
        .push_response(ModelResponse::tool_call(
            "call_del_1",
            "delegate_to_peer",
            json!({
                "peer": "agent://local/kanban",
                "task": "Create issue for auth crash"
            }),
            10,
            10,
        ))
        .await;

    kanban_provider.push_text("Issue LIN-999 created").await;
    dev_provider
        .push_text("I delegated to Kanban and issue LIN-999 was created.")
        .await;

    let answer = dev
        .prompt("Please track this auth crash issue")
        .await
        .unwrap();
    assert!(answer.contains("LIN-999"));
}

// =========================================================================
// 8. 1:1 Handshake (Ping/Pong) & Explicit Session Negotiation (Continuation vs Fresh)
// =========================================================================

#[tokio::test]
async fn peer_ping_pong_handshake_verifies_liveness_and_latency() {
    use acp::Fabric;

    let fabric = Fabric::new("ping-fabric");
    let alice_addr = addr("agent://local/alice");
    let bob_addr = addr("agent://local/bob");

    let alice = Agent::builder(alice_addr.as_str())
        .name("Alice")
        .provider(MockProvider::new())
        .connect_to(&fabric)
        .build()
        .await
        .unwrap();

    let bob = Agent::builder(bob_addr.as_str())
        .name("Bob")
        .provider(MockProvider::new())
        .connect_to(&fabric)
        .build()
        .await
        .unwrap();

    let (bob, bob_inbox) = bob.into_serving().unwrap();
    tokio::spawn(async move {
        let _ = bob.serve(bob_inbox).await;
    });

    // Alice pings Bob to check liveness before sending major tasks
    let latency = alice
        .ping_peer(&bob_addr, Duration::from_secs(3))
        .await
        .unwrap();
    assert!(latency.as_millis() < 3000);
}

#[tokio::test]
async fn peer_session_negotiation_allows_follow_up_continuation_or_fresh_start() {
    use nuo_agent::session::InMemorySessionStore;
    use acp::Fabric;

    let fabric = Fabric::new("session-mesh");
    let caller_addr = addr("agent://local/caller");
    let worker_addr = addr("agent://local/worker");

    let caller_provider = Arc::new(MockProvider::new());
    let worker_provider = Arc::new(MockProvider::new());
    let store = Arc::new(InMemorySessionStore::new());

    let caller = Agent::builder(caller_addr.as_str())
        .name("Caller")
        .provider_arc(caller_provider.clone())
        .connect_to(&fabric)
        .with_p2p_delegation()
        .build()
        .await
        .unwrap();

    let worker = Agent::builder(worker_addr.as_str())
        .name("Worker")
        .provider_arc(worker_provider.clone())
        .connect_to(&fabric)
        .with_store_arc(store)
        .build()
        .await
        .unwrap();

    let (worker, worker_inbox) = worker.into_serving().unwrap();
    tokio::spawn(async move {
        let _ = worker.serve(worker_inbox).await;
    });

    // Turn 1: Caller delegates first task on thread "feature_x"
    worker_provider.push_text("Secret code is 42").await;
    let outcome_1 = caller
        .delegate_with_thread(
            &worker_addr,
            "Remember that the secret code is 42",
            Some("feature_x"),
        )
        .await
        .unwrap();

    assert!(outcome_1.is_resolved());
    assert_eq!(outcome_1.output(), Some("Secret code is 42"));

    let session_handle = outcome_1.session_id().unwrap();
    assert!(session_handle.contains("feature_x"));

    // Turn 2 (Continuation): Caller uses the returned session thread to ask a follow-up
    worker_provider
        .push_text("The secret code you gave me earlier is 42.")
        .await;
    let outcome_2 = caller
        .delegate_with_thread(&worker_addr, "What was the secret code?", Some("feature_x"))
        .await
        .unwrap();

    assert!(outcome_2.is_resolved());
    assert!(outcome_2.output().unwrap().contains("42"));

    // Turn 3 (Fresh Start): Caller decides NOT to continue on thread "feature_x", but on a new thread "fresh_task"
    worker_provider
        .push_text("I have no record of any secret code in this conversation.")
        .await;
    let outcome_3 = caller
        .delegate_with_thread(
            &worker_addr,
            "What was the secret code?",
            Some("fresh_task"),
        )
        .await
        .unwrap();

    assert!(outcome_3.is_resolved());
    assert!(outcome_3.output().unwrap().contains("no record"));
}
