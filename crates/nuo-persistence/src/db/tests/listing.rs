//! Role-scoped listings and switch candidates.

use super::save::save_session_full;
use super::*;

#[test]
fn latest_session_filters_by_workspace_and_persona() {
    use nuo_protocol::WorkspaceFilter;
    let engine = DatabaseEngine::open_in_memory().unwrap();

    let mut unbound = crate::session::SessionData {
        role: Some("philosopher".into()),
        ..Default::default()
    };
    unbound.workspace = None;
    save_session_full(&engine, &unbound).unwrap();

    let mut bound = crate::session::SessionData {
        role: Some("philosopher".into()),
        ..Default::default()
    };
    bound.workspace = Some(nuo_protocol::WorkspaceBinding::new("/repo/x"));
    save_session_full(&engine, &bound).unwrap();

    let unbound_filter = nuo_protocol::ThreadFilter {
        role: Some("philosopher".into()),
        workspace: WorkspaceFilter::Unbound,
    };
    let bound_filter = nuo_protocol::ThreadFilter {
        role: Some("philosopher".into()),
        workspace: WorkspaceFilter::Path("/repo/x".into()),
    };
    assert_eq!(
        session_store::latest_session(&engine.conn, &unbound_filter).unwrap(),
        Some(unbound.id.clone())
    );
    assert_eq!(
        session_store::latest_session(&engine.conn, &bound_filter).unwrap(),
        Some(bound.id.clone())
    );
    assert_eq!(
        session_store::latest_session(
            &engine.conn,
            &nuo_protocol::ThreadFilter {
                role: Some("nobody".into()),
                workspace: WorkspaceFilter::Unbound,
            }
        )
        .unwrap(),
        None
    );
    // Unbound + no role constraint still finds only unbound sessions.
    assert_eq!(
        session_store::latest_session(
            &engine.conn,
            &nuo_protocol::ThreadFilter::workspace_only(WorkspaceFilter::Unbound)
        )
        .unwrap(),
        Some(unbound.id)
    );
}

#[test]
fn role_scoped_switch_candidates_exclude_active() {
    use nuo_protocol::ThreadScope;
    let engine = DatabaseEngine::open_in_memory().unwrap();

    let make_session = |id: &str, role: &str, ws: Option<&str>, time: u64| {
        let mut data = crate::session::SessionData {
            id: id.into(),
            role: Some(role.into()),
            workspace: ws.map(nuo_protocol::WorkspaceBinding::new),
            created_at: time,
            updated_at: time,
            ..Default::default()
        };
        let msg = nuo_protocol::Message::new(nuo_protocol::Role::User, format!("hello from {id}"));
        data.transcript
            .push(nuo_protocol::TranscriptEntry::from_message(0, &msg));
        data
    };

    // Two philosophist sessions (workspace-free)
    let p1 = make_session("phil-1", "philosophist", None, 100);
    let p2 = make_session("phil-2", "philosophist", None, 200);
    // Another role (e.g. translator) with workspace-free
    let t1 = make_session("trans-1", "translator", None, 300);
    // Developer session bound to /repo/a
    let d1 = make_session("dev-1", "developer", Some("/repo/a"), 400);

    save_session_full(&engine, &p1).unwrap();
    save_session_full(&engine, &p2).unwrap();
    save_session_full(&engine, &t1).unwrap();
    save_session_full(&engine, &d1).unwrap();

    let phil_scope = ThreadScope::new("philosophist", None);
    // Active is phil-2: candidates must return phil-1 ONLY, excluding phil-2, trans-1, and dev-1
    let candidates =
        session_store::list_switch_candidates(&engine.conn, &phil_scope.filter(), "phil-2")
            .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].id, "phil-1");

    // Active is phil-1: candidates must return phil-2 ONLY
    let candidates =
        session_store::list_switch_candidates(&engine.conn, &phil_scope.filter(), "phil-1")
            .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].id, "phil-2");

    // Active is unknown: candidates return both phil-2 and phil-1
    let candidates =
        session_store::list_switch_candidates(&engine.conn, &phil_scope.filter(), "none").unwrap();
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].id, "phil-2");
    assert_eq!(candidates[1].id, "phil-1");

    // latest_session_in_scope for philosophist
    assert_eq!(
        session_store::latest_session_in_scope(&engine.conn, &phil_scope).unwrap(),
        Some("phil-2".into())
    );

    // Developer + /repo/a scope. The developer session is the only member.
    let ws_scope = ThreadScope::new("developer", Some("/repo/a".into()));
    let candidates =
        session_store::list_switch_candidates(&engine.conn, &ws_scope.filter(), "dev-1").unwrap();
    assert!(candidates.is_empty()); // Excluded itself, no other candidates
}
