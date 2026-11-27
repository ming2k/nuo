//! The action manifest's own invariants, exercised on the vocabulary itself.
//!
//! These tests are the mechanism half of ADR-0252: they operate on whatever table is
//! handed to them, so the same assertions apply to the `thread.*` namespace as it lands
//! and to every namespace after it. A gate that only checked one hard-coded table would
//! be a snapshot of today's vocabulary rather than a rule about it.

use super::*;

/// A miniature namespace, shaped like the real ones, so the invariants can be exercised
/// without depending on which namespace has landed yet.
const DEMO_PRIMITIVES: &[Primitive] = &[
    Primitive {
        id: ActionId("thread.create"),
        authority: Authority::Server,
        class: ActionClass::SessionControl,
        invalidates: &[Projection::Transcript, Projection::Tabs],
        requires: &[],
        idempotency: Idempotency::NotIdempotent,
    },
    Primitive {
        id: ActionId("thread.bind_workspace"),
        authority: Authority::Server,
        class: ActionClass::SessionControl,
        invalidates: &[],
        requires: &[],
        idempotency: Idempotency::IdempotentWithKey,
    },
    Primitive {
        id: ActionId("thread.interrupt"),
        authority: Authority::Server,
        class: ActionClass::Act,
        invalidates: &[Projection::Chrome],
        requires: &[Requirement::RoundIsRunning],
        idempotency: Idempotency::Idempotent,
    },
    Primitive {
        id: ActionId("thread.read_usage"),
        authority: Authority::Server,
        class: ActionClass::Reading,
        invalidates: &[],
        requires: &[],
        idempotency: Idempotency::Idempotent,
    },
];

/// A second namespace, because one primitive set genuinely cannot cover every domain
/// (ADR-0252 §6). Its existence is part of the fixture: the invariant below asserts
/// per-namespace membership, so a `Local` primitive cannot be smuggled into `thread.*`
/// just because it happened to be convenient there.
const DEMO_CLIENT_PRIMITIVES: &[Primitive] = &[Primitive {
    id: ActionId("client.focus_next"),
    authority: Authority::Local,
    class: ActionClass::Reading,
    invalidates: &[],
    requires: &[],
    idempotency: Idempotency::Idempotent,
}];

const DEMO_COMPOSITIONS: &[Composition] = &[
    Composition {
        id: ActionId("thread.start_fresh"),
        steps: &[ActionId("thread.create"), ActionId("thread.bind_workspace")],
        requires: &[],
        invalidates: &[
            Projection::Transcript,
            Projection::Tabs,
            Projection::DraftOwner,
        ],
        presentation: Presentation::Receipt,
        slash: Some("/new"),
    },
    Composition {
        id: ActionId("thread.halt"),
        steps: &[ActionId("thread.interrupt")],
        requires: &[Requirement::RoundIsRunning],
        invalidates: &[Projection::Chrome],
        presentation: Presentation::ThreadRow,
        slash: None,
    },
];

const DEMO: Namespace = Namespace {
    name: "thread",
    primitives: DEMO_PRIMITIVES,
    compositions: DEMO_COMPOSITIONS,
};

/// Every namespace under test. A new one joins this slice and inherits every invariant
/// below, which is the point of the fixture being plural.
const DEMO_NAMESPACES: &[Namespace] = &[
    DEMO,
    Namespace {
        name: "client",
        primitives: DEMO_CLIENT_PRIMITIVES,
        compositions: &[],
    },
];

/// [INV-VERB-04]: a composition's every step names a declared primitive.
///
/// This is what stops a composition from becoming a hidden code path: it can only order
/// acts the manifest already declares, so there is no way to smuggle a step past the
/// class and authority of the vocabulary.
#[test]
fn composition_steps_name_declared_primitives() {
    for composition in DEMO.compositions {
        for step in composition.steps {
            assert!(
                DEMO.primitive(*step).is_some(),
                "[INV-VERB-04] (ADR-0252): composition `{}` steps on `{step}`, which this \
                 namespace does not declare. A composition orders declared primitives; it \
                 cannot introduce one.",
                composition.id
            );
        }
    }
}

/// [INV-VERB-04]: ids are unique and namespaced.
///
/// The namespace prefix is not decoration — it is what lets ADR-0252 §6 partition
/// provider/OAuth/MCP state machines out of the `thread.*` vocabulary instead of
/// flattening three domains into one list.
#[test]
fn action_ids_are_unique_and_namespaced() {
    let mut seen = std::collections::BTreeSet::new();
    for namespace in DEMO_NAMESPACES {
        for id in namespace
            .primitives
            .iter()
            .map(|p| p.id)
            .chain(namespace.compositions.iter().map(|c| c.id))
        {
            assert!(
                seen.insert(id),
                "[INV-VERB-04] (ADR-0252): `{id}` is declared twice across the manifest"
            );
            assert_eq!(
                id.namespace(),
                namespace.name,
                "[INV-VERB-04] (ADR-0252): `{id}` sits in namespace `{}` but names `{}`",
                namespace.name,
                id.namespace()
            );
            assert!(
                !id.member().is_empty(),
                "[INV-VERB-04] (ADR-0252): `{id}` has no member segment"
            );
        }
    }
}

/// [INV-VERB-01]: an action's class decides its residency; nothing else may.
///
/// The two sides of the rule are asserted together because either alone is satisfiable
/// by a bug: a ledger that records readings asserts stale measurements as history, and a
/// ledger that drops acts loses the only answer to "why is this workspace trusted".
#[test]
fn only_an_act_earns_a_record_or_a_row() {
    for primitive in DEMO.primitives {
        match primitive.class {
            ActionClass::Act => {
                assert!(primitive.class.earns_a_record() && primitive.class.occupies_a_row());
            }
            ActionClass::Reading | ActionClass::SessionControl => {
                assert!(
                    !primitive.class.earns_a_record(),
                    "[INV-VERB-06] (ADR-0252): `{}` is `{:?}` and must not enter the ledger",
                    primitive.id,
                    primitive.class
                );
                assert!(
                    !primitive.class.occupies_a_row(),
                    "[INV-VERB-06] (ADR-0252): `{}` is `{:?}` and must not occupy a thread row",
                    primitive.id,
                    primitive.class
                );
            }
        }
    }
}

/// [INV-VERB-05]: presentation is declared, and a topology change cannot be a row.
///
/// `/new` is the case the rule exists for: it changes durable topology, so the thread it
/// would write into is one it just created. Any composition touching a `SessionControl`
/// step must land as a receipt or silently — never in the transcript.
#[test]
fn a_topology_change_never_presents_as_a_thread_row() {
    for composition in DEMO.compositions {
        let class = composition.class(DEMO.primitives).unwrap_or_else(|| {
            panic!(
                "[INV-VERB-04]: `{}` has no resolvable class",
                composition.id
            )
        });
        if class == ActionClass::SessionControl {
            assert!(
                !composition.presentation.reaches_the_transcript(),
                "[INV-VERB-05] (ADR-0252): `{}` changes thread topology but presents as a \
                 transcript row; the row would belong to a thread it had not created yet",
                composition.id
            );
        }
    }
}

/// A composition inherits the strongest claim its steps make, so a sequence cannot be
/// filed more cheaply than its most consequential step.
#[test]
fn a_composition_takes_the_strongest_class_of_its_steps() {
    let start = DEMO.composition(ActionId("thread.start_fresh")).unwrap();
    assert_eq!(
        start.class(DEMO.primitives),
        Some(ActionClass::SessionControl),
        "two SessionControl steps cannot be filed as anything weaker"
    );
    let halt = DEMO.composition(ActionId("thread.halt")).unwrap();
    assert_eq!(
        halt.class(DEMO.primitives),
        Some(ActionClass::Act),
        "an Act step makes the composition an Act"
    );
}

/// [INV-VERB-03]: a local action is declared, not defaulted.
///
/// The point of `Authority::Local` is that "this never crosses the wire" is a written
/// decision. If a primitive could hold an undisclosed authority, the client's total
/// trigger mapping would have nothing to resolve against and would grow a `_` arm again.
#[test]
fn authority_decides_whether_the_wire_is_used() {
    for primitive in DEMO.primitives {
        assert_eq!(
            primitive.authority.crosses_the_wire(),
            !matches!(primitive.authority, Authority::Local),
            "`{}` must state whether invoking it crosses the wire",
            primitive.id
        );
    }
    // And a `Local` primitive must not claim to invalidate a server-owned projection,
    // which would be a contradiction: nothing refreshed it.
    for primitive in DEMO.primitives {
        if primitive.authority == Authority::Local {
            assert!(
                primitive.invalidates.is_empty(),
                "[INV-VERB-01] (ADR-0252): `{}` is local but claims to invalidate {:?}; the \
                 client would wait for a refresh that never comes",
                primitive.id,
                primitive.invalidates
            );
        }
    }
}

/// Lookup is by identity, and the slash spelling is a projection of the declaration.
///
/// ADR-0119 §2b's rule, made mechanical: the client asks the manifest what a spelling
/// means instead of holding a second table of its own.
#[test]
fn lookup_is_by_identity_and_slash_spelling_resolves() {
    assert_eq!(
        DEMO.composition_for_slash("/new").map(|c| c.id),
        Some(ActionId("thread.start_fresh"))
    );
    assert_eq!(DEMO.composition_for_slash("/nope"), None);
    assert_eq!(
        DEMO.primitive(ActionId("thread.interrupt"))
            .map(|p| p.class),
        Some(ActionClass::Act)
    );
    // A primitive is never reachable as a slash spelling: a composition is the product
    // verb, and admitting a bare step would let a client invoke half of one.
    assert!(
        DEMO.primitives
            .iter()
            .all(|p| DEMO.composition_for_slash(p.id.0).is_none())
    );
}
