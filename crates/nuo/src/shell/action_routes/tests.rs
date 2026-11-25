//! Routing-table invariants ([`INV-VERB-03`], ADR-0252 step 4).
//!
//! These are the tests that make the table a *projection* of the manifest rather than a
//! second copy of it: the manifest half is checked against the declaration, and the
//! wire half is checked for not having become a list.

use super::*;
use server::composition::thread_actions;

/// No spelling is claimed by two rows.
///
/// A duplicate is a routing ambiguity — whichever row happens to be first wins, and which
/// one that is depends on table order, which is exactly the kind of silent preference
/// ADR-0119 §2b's drift came from.
#[test]
fn no_spelling_is_routed_twice() {
    let mut owner: std::collections::BTreeMap<&'static str, &'static str> =
        std::collections::BTreeMap::new();
    for route in ROUTES {
        for spelling in route.spellings {
            if let Some(previous) = owner.insert(spelling, route.canonical()) {
                panic!(
                    "[INV-VERB-03] (ADR-0252): `{spelling}` is claimed by both `{previous}` and \
                     `{}`",
                    route.canonical()
                );
            }
        }
    }
}

/// Every route declares at least one spelling, and the canonical one is first.
#[test]
fn every_route_declares_a_spelling() {
    for route in ROUTES {
        assert!(
            !route.spellings.is_empty(),
            "[INV-VERB-03]: a route with no spelling can never match"
        );
        assert_eq!(
            route.canonical(),
            route.spellings[0],
            "`canonical()` must be the first spelling, not a separate field"
        );
        assert!(
            route.canonical().starts_with('/'),
            "[INV-VERB-03]: `{}` is not spelled as a slash command",
            route.canonical()
        );
    }
}

/// A row that claims a manifest action names a composition whose `slash` is that spelling.
///
/// The bidirectional check. If it fails, either the manifest row is wrong or the
/// declaration moved — both are drift, and both are silent without this test.
#[test]
fn a_manifest_route_names_its_own_composition() {
    let mut claimed = 0usize;
    for route in ROUTES {
        let RouteSource::Manifest(id) = route.source else {
            continue;
        };
        claimed += 1;
        let composition = thread_actions::NAMESPACE
            .composition(id)
            .unwrap_or_else(|| {
                panic!(
                    "[INV-VERB-03] (ADR-0252): `{}` claims manifest action `{id}`, which \
                     `thread.*` does not declare",
                    route.canonical()
                )
            });
        assert_eq!(
            composition.slash,
            Some(route.spellings[0]),
            "[INV-VERB-03] (ADR-0252): `{}` routes to `{id}`, but that composition is spelled \
             {:?} in the manifest",
            route.spellings[0],
            composition.slash
        );
        // An alias must not shadow a *different* composition's canonical spelling; the
        // duplicate check above covers collisions, and this covers the semantic half: an
        // alias is the same action, so it must resolve to the same declaration.
        for alias in &route.spellings[1..] {
            assert_ne!(
                thread_actions::NAMESPACE
                    .composition_for_slash(alias)
                    .map(|c| c.id),
                Some(id),
                "`{alias}` is an alias of `{id}`, so the manifest must not also spell `{id}` \
                 that way"
            );
        }
    }
    assert!(
        claimed > 0,
        "[INV-VERB-03]: no row claims a manifest action, so this test certifies nothing"
    );
}

/// Every manifest composition that carries a `slash` is reachable from the composer.
///
/// The other direction: a declaration with no way to reach it is a dead row, and a client
/// that offers completion for it would advertise an action it cannot invoke.
#[test]
fn every_slash_composition_has_a_route() {
    for composition in thread_actions::COMPOSITIONS {
        let Some(slash) = composition.slash else {
            continue;
        };
        let route = route_for(slash, slash).unwrap_or_else(|| {
            panic!(
                "[INV-VERB-03] (ADR-0252): composition `{}` is spelled `{slash}` but no route \
                 claims it",
                composition.id
            )
        });
        assert_eq!(
            route.source_target_id(),
            Some(composition.id),
            "`{slash}` resolves to a different action than the one it declares"
        );
    }
}

/// A client-only row states why the server must not know it.
///
/// ADR-0041 `[INV-CMD-01]` keeps workspace views out of the thread vocabulary. That is an
/// allowance, and an allowance without a reason is indistinguishable from an oversight.
#[test]
fn a_client_only_route_states_its_reason() {
    for route in ROUTES {
        if let RouteSource::ClientOnly { reason } = route.source {
            assert!(
                !reason.trim().is_empty(),
                "[INV-CMD-01] (ADR-0041): `{}` is client-owned with no stated reason",
                route.canonical()
            );
        }
    }
}

/// An unclaimed spelling is forwarded verbatim rather than reinterpreted.
///
/// This is ADR-0119 §2 as an assertion: admission belongs to the dispatcher. A typo must
/// reach the server and be rejected there, not become a client-side guess.
#[test]
fn an_unclaimed_spelling_is_forwarded_verbatim() {
    assert!(matches!(
        resolve("/modles"),
        InputAction::SendSlash(text) if text == "/modles"
    ));
    // And a known-but-surfaceless command takes the same path, which is why the table
    // does not need to list it.
    assert!(matches!(
        resolve("/compact now"),
        InputAction::SendSlash(text) if text == "/compact now"
    ));
}

/// A claimed spelling resolves to its action, with and without arguments, and aliases
/// resolve identically.
#[test]
fn a_claimed_spelling_resolves_with_arguments_and_aliases() {
    for (draft, expected) in [
        ("/new", "CreateNewSession"),
        ("/new optimize the retry loop", "SendSlash"),
        ("/thread", "CreateNewSession"),
        ("/dashboard", "NavigateDashboard"),
        ("/models gpt-4o", "OpenModels"),
        ("/quota", "OpenQuotas"),
        ("/quotas", "OpenQuotas"),
    ] {
        let resolved = resolve(draft);
        assert_eq!(
            format!("{resolved:?}")
                .split(['(', ' '])
                .next()
                .unwrap_or("?"),
            expected,
            "`{draft}` resolved to {resolved:?}"
        );
    }
}

/// `ArgsPolicy::ExactOnly` matches only as the whole draft.
///
/// The one row that needs it, asserted in both directions so the policy cannot quietly
/// become the default: `/context` opens the inspector, `/context --json` is the machine
/// form and reaches the server (ADR-0220 §7).
#[test]
fn an_exact_only_route_rejects_arguments() {
    assert!(matches!(resolve("/context"), InputAction::OpenContextPlan));
    assert!(matches!(
        resolve("/context --json"),
        InputAction::SendSlash(text) if text == "/context --json"
    ));
}
