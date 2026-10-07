//! `nuo session …` (ADR-0116): the session noun. Removing lives here;
//! *joining* a session is `nuo attach` (a top-level verb — it is the
//! primary interactive act, not a sub-management task), and *listing* is
//! `nuo status` — the session table is the server's view of what
//! it hosts, so a `session ls` would duplicate it verbatim.

use crate::cli::SessionAction;
use nuo_client as client;
use nuo::serve::ControlRequest;
use std::path::PathBuf;

pub async fn run(
    action: SessionAction,
    project_override: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let project_root = project_override
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    match action {
        SessionAction::Delete(id) => {
            let info = client::discover(&project_root).ok_or_else(|| {
                "no server is running. Start or discover one before managing sessions.".to_string()
            })?;
            if !client::versions_compatible(&info) {
                return Err(client::incompatibility_error(&info).into());
            }
            client::control(
                &info,
                ControlRequest::KillSession {
                    session_id: id.clone(),
                },
            )
            .await?;
            println!("Session '{id}' has been terminated.");
        }
    }
    Ok(())
}
