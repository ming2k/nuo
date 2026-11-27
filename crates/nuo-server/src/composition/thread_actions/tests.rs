//! `thread.*` namespace invariants, checked on the namespace the server actually
//! publishes.
//!
//! The fixture-based tests in `nuo_protocol::action::tests` prove the *rules*; these prove
//! the *declaration* obeys them. Both are needed: a rule that holds on a fixture and not
//! on the shipped table is a rule nobody enforces.

use super::*;

/// [INV-VERB-04]: every step of every composition names a declared primitive.
///
/// A composition orders acts the manifest already declares. If it could name an
/// undeclared id, it would be a hidden code path: the step would never be subject to a
/// class or an authority.
#[test]
fn every_step_names_a_declared_primitive() {
    for composition in COMPOSITIONS {
        for step in composition.steps {
            assert!(
                NAMESPACE.primitive(*step).is_some(),
                "[INV-VERB-04] (ADR-0252): `{}` steps on `{step}`, which `thread.*` does not \
                 declare",
                composition.id
            );
        }
    }
}

/// [INV-VERB-04]: one declaration per id, and every id carries this namespace.
#[test]
fn ids_are_unique_and_namespaced() {
    let mut seen = std::collections::BTreeSet::new();
    for id in PRIMITIVES
        .iter()
        .map(|p| p.id)
        .chain(COMPOSITIONS.iter().map(|c| c.id))
    {
        assert!(seen.insert(id), "[INV-VERB-04]: `{id}` is declared twice");
        assert_eq!(
            id.namespace(),
            NAMESPACE.name,
            "[INV-VERB-04]: `{id}` does not sit in `{}`",
            NAMESPACE.name
        );
    }
}

/// [INV-VERB-05]: no topology change presents as a transcript row.
///
/// This is the `/new` defect stated as a gate. `start_fresh` creates a thread, so a row
/// it wrote would belong to a session minted by the same action — the row's own thread
/// did not exist when the action ran.
#[test]
fn no_topology_change_presents_as_a_thread_row() {
    for composition in COMPOSITIONS {
        let class = composition
            .class(PRIMITIVES)
            .expect("every step is declared, so a class resolves");
        if class != ActionClass::SessionControl {
            continue;
        }
        assert!(
            !composition.presentation.reaches_the_transcript(),
            "[INV-VERB-05] (ADR-0252): `{}` changes thread topology but presents as a \
             transcript row",
            composition.id
        );
    }
    // Named explicitly, because this is the row the defect was reported against.
    let start_fresh = NAMESPACE
        .composition(ActionId("thread.start_fresh"))
        .expect("start_fresh is declared");
    assert_eq!(start_fresh.presentation, Presentation::Receipt);
}

/// [INV-VERB-06]: only an `Act` settles a row or earns a record.
#[test]
fn only_an_act_is_a_row() {
    for primitive in PRIMITIVES {
        assert_eq!(
            primitive.class.occupies_a_row(),
            primitive.class == ActionClass::Act,
            "[INV-VERB-06]: `{}` disagrees with its own class",
            primitive.id
        );
    }
    // `interrupt` is the one act in this namespace today, and it *is* part of the
    // thread's story; `create` mints the story and `end` closes it, so neither is.
    assert_eq!(
        NAMESPACE
            .primitive(ActionId("thread.interrupt"))
            .map(|p| p.class),
        Some(ActionClass::Act)
    );
    for id in ["thread.create", "thread.end", "thread.read_status"] {
        assert_ne!(
            NAMESPACE.primitive(ActionId(id)).map(|p| p.class),
            Some(ActionClass::Act),
            "`{id}` must not settle a thread row"
        );
    }
}

/// [INV-VERB-03]: the slash spelling resolves through the manifest.
///
/// ADR-0119 §2b's single-owner rule, checked on the shipped table: `/new` is answered by
/// the declaration, so the client's hand-written intercept match has something correct to
/// become.
#[test]
fn slash_spellings_resolve_through_the_manifest() {
    assert_eq!(
        NAMESPACE.composition_for_slash("/new").map(|c| c.id),
        Some(ActionId("thread.start_fresh"))
    );
    assert_eq!(
        NAMESPACE.composition_for_slash("/exit").map(|c| c.id),
        Some(ActionId("thread.end_foreground"))
    );
    // A primitive is never directly spellable: invoking half a composition is how the
    // five divergent "create a thread" implementations happened.
    for primitive in PRIMITIVES {
        assert!(
            NAMESPACE.composition_for_slash(primitive.id.0).is_none(),
            "[INV-VERB-03]: the primitive `{}` is directly spellable",
            primitive.id
        );
    }
}

/// Ownership: a topology primitive is performed by the server, and a `Local` primitive
/// may not claim to invalidate a server-owned projection.
///
/// `thread.*` has no `Local` member today — navigation and editing are the client's
/// namespace — and this asserts that the boundary is not blurred by convenience.
#[test]
fn every_thread_primitive_is_server_or_surface_owned() {
    for primitive in PRIMITIVES {
        assert_ne!(
            primitive.authority,
            Authority::Local,
            "[INV-VERB-01]: `{}` is thread topology and cannot be local-only",
            primitive.id
        );
        assert!(
            primitive.authority.crosses_the_wire(),
            "`{}` must reach the server",
            primitive.id
        );
    }
}

/// [INV-VERB-02]: `start_fresh` has no failure precondition (it is valid on both empty
/// and non-empty threads) and declares every projection the switch invalidates.
#[test]
fn start_fresh_declares_invalidations_without_spurious_precondition() {
    let start_fresh = NAMESPACE
        .composition(ActionId("thread.start_fresh"))
        .expect("start_fresh is declared");
    assert!(
        start_fresh.requires.is_empty(),
        "[INV-VERB-02] (ADR-0252): `start_fresh` is valid on both empty and non-empty \
         threads; emptiness is a branching policy, not a failure precondition"
    );
    // And it declares every projection the switch invalidates — the list a client needs
    // when one owner per fact (ADR-0253) replaces reconciliation.
    for projection in [
        Projection::Transcript,
        Projection::Tabs,
        Projection::DraftOwner,
    ] {
        assert!(
            start_fresh.invalidates.contains(&projection),
            "`start_fresh` must invalidate {projection:?}"
        );
    }
}

/// [INV-VERB-04]: every slash-bearing composition in `thread.*` agrees with
/// `define_builtin_commands!` on spelling and effect class.
///
/// This ties the manifest to the live server builtin table so `thread_actions` cannot
/// drift into an unwired static copy.
#[test]
fn slash_compositions_agree_with_builtin_commands() {
    use crate::composition::startup::BuiltinCmd;

    for composition in COMPOSITIONS {
        let Some(slash) = composition.slash else {
            continue;
        };
        let spec = BuiltinCmd::find_spec(slash).unwrap_or_else(|| {
            panic!(
                "[INV-VERB-04] (ADR-0252): composition `{}` declares slash `{slash}`, \
                 which is missing from `define_builtin_commands!`",
                composition.id
            )
        });
        let manifest_class = composition
            .class(PRIMITIVES)
            .expect("composition has declared steps");
        let builtin_class = ActionClass::from(spec.effect);
        assert_eq!(
            manifest_class, builtin_class,
            "[INV-VERB-04] (ADR-0252): `{slash}` has class {manifest_class:?} in manifest \
             `{}` but {builtin_class:?} in `define_builtin_commands!`",
            composition.id
        );
    }
}
