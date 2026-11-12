//! Fork lineage end-to-end and migration-27 backfill.

use super::*;

/// Migration 27 backfills facts for sessions created before the live mirror.
///
/// Those sessions have a complete transcript and an empty graph. Compaction reads
/// the graph, so without this they keep growing until the provider rejects the
/// request — the mirror only fills in on their next commit, and a session at the
/// context ceiling cannot reach a next commit.

#[test]
fn migration_27_backfills_the_fact_graph_from_the_transcript() {
    let engine = DatabaseEngine::open_in_memory().unwrap();
    // Hand-build the legacy state directly (thread + membership rows) rather than
    // going through the save path: this test is about the *migration*, and using
    // today's writer would prove nothing about a database written by an older
    // binary.
    engine
        .conn
        .execute_batch(
            "INSERT INTO threads (id, created_at_ms, updated_at_ms) VALUES ('legacy', 1000, 2000);",
        )
        .expect("thread row");
    for (seq, role, text) in [
        (0i64, "user", "old question"),
        (1, "assistant", "old answer"),
    ] {
        engine
            .conn
            .execute(
                "INSERT INTO entries (id, kind, role, content, created_at_ms, payload)
                 VALUES (?1, 'message', ?2, ?3, 1000, '{}')",
                rusqlite::params![format!("entry-{seq}"), role, text],
            )
            .expect("entry row");
        engine
            .conn
            .execute(
                "INSERT INTO entry_memberships (session_id, seq, entry_id) VALUES ('legacy', ?1, ?2)",
                rusqlite::params![seq, format!("entry-{seq}")],
            )
            .expect("membership row");
    }

    let before: i64 = engine
        .conn
        .query_row("SELECT COUNT(*) FROM causal_nodes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, 0, "precondition: legacy session has no facts");

    // Re-run the backfill exactly as the migration does.
    {
        let tx = engine.conn.unchecked_transaction().expect("transaction");
        apply_causal_fact_backfill_schema(&tx).expect("backfill must apply");
        tx.commit().expect("commit");
    }

    let after: i64 = engine
        .conn
        .query_row(
            "SELECT COUNT(*) FROM causal_nodes WHERE session_id = 'legacy'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(after, 2, "both transcript messages became facts");

    // The decisive check: the graph must now hydrate through the *real* reader,
    // which fails the whole hydrate on a payload it cannot decode.
    let ir = crate::db::session_ir::load_session_ir(&engine.conn, "legacy")
        .expect("backfilled graph must be loadable")
        .expect("session exists");
    assert_eq!(ir.history.nodes.len(), 2);
    // Every backfilled payload must be decodable — that is the whole point, since
    // one undecodable node fails the entire hydrate.
    let messages = ir.resolve_active_messages();
    assert!(
        !messages.is_empty(),
        "backfilled payloads must decode: {messages:#?}"
    );
    // Honest limitation: the backfill cannot invent parent edges (a database with
    // forked branches has no safe way to tell one lineage's predecessor from
    // another's), so `resolve_active_messages` walks back only to the newest node
    // until that session commits again and the live mirror repairs the chain. The
    // facts themselves are all present and correct.
    assert_eq!(
        ir.history.nodes.len(),
        2,
        "all transcript entries are facts, reachable or not"
    );
    let payloads: Vec<String> = ir
        .history
        .nodes
        .values()
        .map(|node| format!("{:?}", node.payload))
        .collect();
    assert!(
        payloads.iter().any(|p| p.contains("old question")),
        "every backfilled payload decodes: {payloads:#?}"
    );

    // Idempotence: a second run changes nothing.
    {
        let tx = engine.conn.unchecked_transaction().expect("transaction");
        apply_causal_fact_backfill_schema(&tx).expect("backfill must be re-appliable");
        tx.commit().expect("commit");
    }
    let again: i64 = engine
        .conn
        .query_row(
            "SELECT COUNT(*) FROM causal_nodes WHERE session_id = 'legacy'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(again, 2, "the backfill is idempotent");
}
