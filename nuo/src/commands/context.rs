//! `nuo context migrate` — the offline legacy→canonical conversion surface
//! (ADR-0280 §4).
//!
//! This command is the only entry point to the migration tool. It runs outside
//! the daemon: it never starts the runtime, never opens the live single-writer
//! door, and never writes the legacy database (it is opened read-only). After
//! it reports, the operator switches the primary database pointer and the new
//! runtime rejects the legacy schema.

use crate::cli::ContextAction;
use std::path::Path;

/// Run a `nuo context …` action.
pub fn run(action: ContextAction) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        ContextAction::Migrate { legacy, target } => {
            let legacy_path = Path::new(&legacy);
            let target_path = Path::new(&target);
            if !legacy_path.exists() {
                return Err(format!("legacy database not found: {legacy}").into());
            }
            if target_path.exists() {
                return Err(format!(
                    "target database already exists: {target} (choose a fresh path; migration never overwrites)"
                )
                .into());
            }
            let report =
                nuo_persistence::db::migration_tool::migrate_files(legacy_path, target_path)?;

            println!(
                "Migrated {} session(s) into {}",
                report.sessions.len(),
                target
            );
            let mut total_facts = 0u64;
            let mut total_unknown = 0u64;
            let mut total_unavailable = 0u64;
            for session in &report.sessions {
                total_facts += session.facts_migrated;
                total_unknown += session.unknown_state;
                total_unavailable += session.unavailable_raw;
                let status = if session.quarantined {
                    "QUARANTINED"
                } else {
                    "ok"
                };
                println!(
                    "  {} [{}] facts={} unknown={} unavailable={} digest={}",
                    session.session_id,
                    status,
                    session.facts_migrated,
                    session.unknown_state,
                    session.unavailable_raw,
                    if session.digest.is_empty() {
                        "-"
                    } else {
                        &session.digest[..12.min(session.digest.len())]
                    }
                );
            }
            println!(
                "\nTotals: {total_facts} fact(s), {total_unknown} unknown-state, {total_unavailable} unavailable-raw"
            );
            if !report.conflicts.is_empty() {
                println!("\nConflicts (quarantined, not resolved):");
                for conflict in &report.conflicts {
                    println!("  {}: {}", conflict.session_id, conflict.reason);
                }
            }
            if !report.complete {
                println!(
                    "\nMigration is INCOMPLETE: {} session(s) quarantined. Resolve the \
                     conflicts or handle them explicitly before switching the primary database.",
                    report.conflicts.len()
                );
                std::process::exit(1);
            }
            println!(
                "\nMigration complete. Verify the report, then switch the primary \
                 database pointer and schema version.\nRun `muta context verify --db {target}` to check integrity."
            );
            Ok(())
        }
        ContextAction::Verify { db, json } => {
            let db_path = Path::new(&db);
            if !db_path.exists() {
                return Err(format!("database not found: {db}").into());
            }
            let report = nuo_persistence::db::migration_tool::verify_files(db_path)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("Verification of {db}");
                for session in &report.sessions {
                    println!(
                        "  {} facts={} purged={} artifact_refs={} dangling={} digest={}",
                        session.session_id,
                        session.facts,
                        session.purged_facts,
                        session.artifact_refs,
                        session.dangling_artifact_refs,
                        &session.digest[..12.min(session.digest.len())]
                    );
                    for problem in &session.problems {
                        println!("      PROBLEM: {problem}");
                    }
                }
                if report.ok {
                    println!("\nOK: no integrity problems found.");
                } else {
                    println!("\nFAILED: integrity problems found.");
                }
            }
            if !report.ok {
                std::process::exit(1);
            }
            Ok(())
        }
    }
}
