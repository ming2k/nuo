//! Integration tests for local spend circuit breaker and model pricing (ADR-0065).

// Integration test target: assertions fail by panicking, so the production
// no-panic lints are meaningless here (the exemption ends at the test boundary).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nuo_agent::agent::{Agent, SpendCircuitBreaker};
use nuo_agent::error::AgentError;
use nuo_agent::provider::{MockProvider, ModelResponse, TokenUsage};
use nuo_wire::budget::{BudgetPeriod, ConnectionBudget};
use nuo_wire::pricing::{ModelPricing, PricingTier};

#[tokio::test]
async fn test_agent_spend_circuit_breaker_trips_and_intercepts_preflight() {
    let provider = MockProvider::new();

    // 1. Setup pricing: $10.00 / 1M input, $20.00 / 1M output
    let pricing = ModelPricing::single(PricingTier {
        context_threshold: None,
        input_standard_nano_per_m: 10_000_000_000,
        cache_read_nano_per_m: 0,
        cache_write_nano_per_m: 0,
        output_standard_nano_per_m: 20_000_000_000,
    });

    // 2. Setup local budget of $0.05 (Session scope)
    let budget = ConnectionBudget::local(0.05, BudgetPeriod::Session);
    let breaker = SpendCircuitBreaker::new(budget);

    // 3. Turn 1: 2,000 prompt + 1,000 completion = 20,000,000 + 20,000,000 = $0.040
    let mut resp1 = ModelResponse::text("First answer", 2000, 1000);
    resp1.usage = TokenUsage {
        prompt_tokens: 2000,
        completion_tokens: 1000,
        total_tokens: 3000,
        cached_tokens: 0,
    };
    provider.push_response(resp1).await;

    // 4. Turn 2: 1,000 prompt + 1,000 completion = 10,000,000 + 20,000,000 = $0.030 -> cumulative $0.070 (trips!)
    let mut resp2 = ModelResponse::text("Second answer", 1000, 1000);
    resp2.usage = TokenUsage {
        prompt_tokens: 1000,
        completion_tokens: 1000,
        total_tokens: 2000,
        cached_tokens: 0,
    };
    provider.push_response(resp2).await;

    let agent = Agent::builder("agent://local/budget-guarded")
        .provider(provider)
        .with_model_pricing(pricing)
        .with_spend_breaker(breaker)
        .build()
        .await
        .unwrap();

    let res1 = agent.prompt("Hello turn 1").await.unwrap();
    assert_eq!(res1, "First answer");

    let res2 = agent.prompt("Hello turn 2").await.unwrap();
    assert_eq!(res2, "Second answer");

    // At this point, cumulative spend is $0.07, surpassing the $0.05 budget limit.
    // Turn 3 should be intercepted at PRE-FLIGHT before touching the provider.
    let err3 = agent.prompt("Hello turn 3").await.unwrap_err();
    match err3 {
        AgentError::SpendBudgetExceeded {
            limit_usd,
            spend_usd,
            period,
        } => {
            assert_eq!(limit_usd, "$0.05");
            assert_eq!(spend_usd, "$0.07");
            assert_eq!(period, "Session");
        }
        other => panic!("expected SpendBudgetExceeded error, got {other:?}"),
    }
}
