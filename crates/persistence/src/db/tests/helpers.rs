use super::*;

pub(super) fn save_session_full(
    engine: &DatabaseEngine,
    data: &crate::thread::ThreadData,
) -> std::result::Result<(), crate::db::SaveError> {
    let prepared = crate::db::thread_session::prepare_save(
        data.clone(),
        crate::db::SaveMode::FullRewrite,
        &[],
        &crate::db::CommitGuard::default(),
        None,
        None,
    )?;
    crate::db::thread_session::save_session_inner(&engine.conn, &prepared).map(|_| ())
}

pub(super) fn load_session_full(
    engine: &DatabaseEngine,
    id: &str,
) -> rusqlite::Result<Option<crate::thread::ThreadData>> {
    crate::db::thread_session::load_session_full(&engine.conn, id)
}
