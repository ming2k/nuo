use super::*;

pub(super) fn save_session_full(
    engine: &DatabaseEngine,
    data: &crate::thread::ThreadData,
) -> std::result::Result<(), crate::db::SaveError> {
    crate::db::thread_session::save_session_inner(
        &engine.conn,
        engine.blob_store.as_ref(),
        data,
        true,
        &[],
        &crate::db::CommitGuard::default(),
    )
    .map(|_| ())
}

pub(super) fn load_session_full(
    engine: &DatabaseEngine,
    id: &str,
) -> rusqlite::Result<Option<crate::thread::ThreadData>> {
    crate::db::thread_session::load_session_full(&engine.conn, id)
}
