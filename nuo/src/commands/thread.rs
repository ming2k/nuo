//! `nuo thread …` (ADR-0043, ADR-0048): thread management.
//!
//! Listing is `nuo thread ls`, administrative termination is `nuo thread rm <id>`.

use crate::cli::ThreadAction;
use nuo_tui::client::{self, ControlRequest};
use nuo_wire::{MonitorAction, MonitorEvent, MonitoredSession};
use std::path::PathBuf;

pub async fn run(
    action: ThreadAction,
    project_override: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let project_root = project_override
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    match action {
        ThreadAction::Open(id) => {
            let mut tui_args = nuo_tui::cli::parse(&[])?;
            tui_args.project = project_override;
            tui_args.mode = nuo_tui::cli::Mode::Attach { id };
            nuo_tui::runner::run_with_options(tui_args).await
        }
        ThreadAction::List { watch, json, all } => {
            let info = match client::discover(&project_root) {
                Some(info) => info,
                None => {
                    if json {
                        println!("[]");
                    } else {
                        eprintln!(
                            "nuo: no server is running. Start one with `nuo` or `nuo server start`."
                        );
                    }
                    return Ok(());
                }
            };
            if !client::versions_compatible(&info) {
                return Err(client::incompatibility_error(&info).into());
            }

            let action = MonitorAction {
                watch,
                include_idle: all,
            };
            let mut rx = client::monitor_stream(&info, action).await?;
            let mut state = match rx.recv().await {
                Some(MonitorEvent::Snapshot(snapshot)) => snapshot,
                Some(_) => return Err("monitor stream opened without a snapshot".into()),
                None => return Err("server closed the monitor stream".into()),
            };

            render_threads(&state.sessions, watch, json);
            if !watch {
                return Ok(());
            }

            while let Some(event) = rx.recv().await {
                match event {
                    MonitorEvent::Snapshot(snapshot) => state = snapshot,
                    MonitorEvent::SessionAdded(row) | MonitorEvent::SessionUpdated(row) => {
                        client::upsert_session_row(&mut state.sessions, row);
                    }
                    MonitorEvent::SessionRemoved { session_id } => {
                        state.sessions.retain(|row| row.id != session_id);
                    }
                    MonitorEvent::ServerDraining => {
                        if !json {
                            eprintln!("nuo: server is shutting down; watch ended.");
                        }
                        return Ok(());
                    }
                    _ => {}
                }
                render_threads(&state.sessions, watch, json);
            }
            Ok(())
        }
        ThreadAction::Delete(id) => {
            let info = client::discover(&project_root).ok_or_else(|| {
                "no server is running. Start or discover one before managing threads.".to_string()
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
            println!("Thread '{id}' has been terminated.");
            Ok(())
        }
    }
}

fn render_threads(sessions: &[MonitoredSession], watch: bool, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string(sessions).unwrap_or_else(|_| "[]".to_string())
        );
        return;
    }
    if watch {
        print!("\x1b[2J\x1b[H");
    }
    println!("{}", format_threads_table(sessions));
}

pub fn format_threads_table(sessions: &[MonitoredSession]) -> String {
    let mut out = String::new();
    if sessions.is_empty() {
        out.push_str("  (no active threads hosted on server; run `nuo thread ls -a` to include reaped/suspended threads)\n");
        return out;
    }

    out.push_str(&format!(
        "  {:<12} {:<12} {:<9} {:>8} {:<10} {}\n",
        "THREAD ID", "STATUS", "ROUNDS", "TOKENS", "UPDATED", "TITLE"
    ));
    for row in sessions {
        let tokens = if let Some(ctx) = row.context_tokens {
            fmt_k(ctx)
        } else if row.output_tokens > 0 {
            fmt_k(row.output_tokens as usize)
        } else {
            "–".to_string()
        };
        let updated = fmt_relative(row.updated_at);
        let rounds = match (row.round, row.turn) {
            (0, _) => "–".to_string(),
            (r, Some(t)) => format!("{r} › {t}"),
            (r, None) => format!("{r}"),
        };
        let title = truncate(&row.overview, 50);

        out.push_str(&format!(
            "  {:<12} {:<12} {:<9} {:>8} {:<10} {}\n",
            short_id(&row.id),
            row.status.as_str(),
            rounds,
            tokens,
            updated,
            title,
        ));
    }
    out
}

fn short_id(id: &str) -> String {
    id.chars().take(10).collect()
}

fn fmt_k(tokens: usize) -> String {
    if tokens >= 1000 {
        format!("{:.1}k", tokens as f64 / 1000.0)
    } else {
        tokens.to_string()
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn fmt_relative(updated_epoch_ms: u64) -> String {
    if updated_epoch_ms == 0 {
        return "–".to_string();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    if now < updated_epoch_ms {
        return "Just now".to_string();
    }
    let elapsed_secs = (now - updated_epoch_ms) / 1000;
    if elapsed_secs < 60 {
        "Just now".to_string()
    } else if elapsed_secs < 3600 {
        format!("{}m ago", elapsed_secs / 60)
    } else if elapsed_secs < 86400 {
        format!("{}h ago", elapsed_secs / 3600)
    } else {
        format!("{}d ago", elapsed_secs / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_wire::{SessionForkKind, SessionHosting, SessionStatus};

    #[test]
    fn format_threads_table_renders_header_and_rows() {
        let rows = vec![MonitoredSession {
            id: "thr_99998888".into(),
            title: Some("implement universal snapshot".into()),
            overview: "implement universal snapshot".into(),
            created_at: 100,
            updated_at: 100,
            message_count: 5,
            hosting: SessionHosting::Hosted,
            status: SessionStatus::Running,
            round: 2,
            turn: Some(1),
            output_tokens: 500,
            elapsed_ms: 10_000,
            current_tool: None,
            activity: None,
            context_tokens: Some(12_500),
            note: None,
            project_root: "/data".into(),
            parent_id: None,
            fork_kind: SessionForkKind::default(),
            digest: None,
        }];
        let formatted = format_threads_table(&rows);
        assert!(formatted.contains("THREAD ID"));
        assert!(formatted.contains("thr_999988"));
        assert!(formatted.contains("running"));
        assert!(formatted.contains("2 › 1"));
        assert!(formatted.contains("12.5k"));
        assert!(formatted.contains("implement universal snapshot"));
    }

    #[test]
    fn empty_threads_table_gives_actionable_tip() {
        let formatted = format_threads_table(&[]);
        assert!(formatted.contains("no active threads"));
        assert!(formatted.contains("nuo thread ls -a"));
    }
}
