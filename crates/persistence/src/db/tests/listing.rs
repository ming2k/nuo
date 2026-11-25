//! Orthogonal thread listings and switch candidates (ADR-0254).

use super::helpers::save_session_full;
use super::*;

#[test]
fn latest_session_filters_orthogonally_by_role_and_workspace() {
    let engine = DatabaseEngine::open_in_memory().unwrap();

    let mut unbound = crate::thread::ThreadData {
        role: Some("philosopher".into()),
        created_at: 100,
        updated_at: 100,
        ..Default::default()
    };
    unbound.workspace = None;
    save_session_full(&engine, &unbound).unwrap();

    let mut bound = crate::thread::ThreadData {
        role: Some("philosopher".into()),
        created_at: 200,
        updated_at: 200,
        ..Default::default()
    };
    bound.workspace = Some(protocol::WorkspaceBinding::new("/repo/x"));
    save_session_full(&engine, &bound).unwrap();

    // Role-only filter spans across workspaces (newest wins: `bound`).
    let role_only = protocol::ThreadFilter::for_role("philosopher");
    assert_eq!(
        thread_session::latest_session(&engine.conn, &role_only).unwrap(),
        Some(bound.id.clone())
    );

    // Role + workspace intersection matches only `bound`.
    let bound_filter = protocol::ThreadFilter {
        role: Some("philosopher".into()),
        workspace: Some("/repo/x".into()),
    };
    assert_eq!(
        thread_session::latest_session(&engine.conn, &bound_filter).unwrap(),
        Some(bound.id.clone())
    );

    // Unknown role returns None.
    assert_eq!(
        thread_session::latest_session(&engine.conn, &protocol::ThreadFilter::for_role("nobody"))
            .unwrap(),
        None
    );

    // Workspace-only filter matches all roles in `/repo/x`.
    assert_eq!(
        thread_session::latest_session(
            &engine.conn,
            &protocol::ThreadFilter::for_workspace("/repo/x")
        )
        .unwrap(),
        Some(bound.id)
    );
}

#[test]
fn orthogonal_switch_candidates_exclude_active() {
    let engine = DatabaseEngine::open_in_memory().unwrap();

    let make_session = |id: &str, role: &str, ws: Option<&str>, time: u64| {
        let mut data = crate::thread::ThreadData {
            id: id.into(),
            role: Some(role.into()),
            workspace: ws.map(protocol::WorkspaceBinding::new),
            created_at: time,
            updated_at: time,
            ..Default::default()
        };
        let msg = tool::Message::new(tool::Role::User, format!("hello from {id}"));
        data.transcript
            .push(protocol::TranscriptEntry::from_message(0, &msg));
        data
    };

    let p1 = make_session("phil-1", "philosophist", None, 100);
    let p2 = make_session("phil-2", "philosophist", None, 200);
    let t1 = make_session("trans-1", "translator", None, 300);
    let d1 = make_session("dev-1", "developer", Some("/repo/a"), 400);
    let r1 = make_session("rev-1", "reviewer", Some("/repo/a"), 450);

    save_session_full(&engine, &p1).unwrap();
    save_session_full(&engine, &p2).unwrap();
    save_session_full(&engine, &t1).unwrap();
    save_session_full(&engine, &d1).unwrap();
    save_session_full(&engine, &r1).unwrap();

    // Role filter: philosophist returns phil-1 when phil-2 is active.
    let phil_filter = protocol::ThreadFilter::for_role("philosophist");
    let candidates =
        thread_session::list_switch_candidates(&engine.conn, &phil_filter, "phil-2").unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].id, "phil-1");

    // Workspace filter: /repo/a returns all roles in /repo/a except active.
    let ws_filter = protocol::ThreadFilter::for_workspace("/repo/a");
    let candidates =
        thread_session::list_switch_candidates(&engine.conn, &ws_filter, "dev-1").unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].id, "rev-1");

    // Unfiltered returns all threads except active.
    let candidates = thread_session::list_switch_candidates(
        &engine.conn,
        &protocol::ThreadFilter::all(),
        "rev-1",
    )
    .unwrap();
    assert_eq!(candidates.len(), 4);
}
