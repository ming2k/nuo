//! `nuo thread …` (ADR-0043, ADR-0048, ADR-0170, ADR-0171): role-scoped
//! thread history.
//!
//! The scope is `(role, workspace binding)`. `--role` defaults to `developer`,
//! whose workspace policy binds it to the current project. `ls` reads durable
//! history, `stop` ends a resident driver and keeps history, and `rm` deletes
//! history permanently.

use crate::cli::ThreadAction;
use nuo::transport as client;
use nuo_protocol::{MonitoredThread, ThreadFilter, ThreadRemovalReport, WorkspaceFilter};
use std::path::PathBuf;

#[cfg(feature = "interactive")]
async fn open_thread(
    id: Option<String>,
    role: Option<String>,
    project_override: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut tui_args = nuo::shell::cli::parse(&[])?;
    tui_args.project = project_override;
    tui_args.role = role;
    tui_args.mode = nuo::shell::cli::Mode::Attach { id };
    nuo::shell::runner::run_with_options(tui_args).await
}

#[cfg(not(feature = "interactive"))]
async fn open_thread(
    _id: Option<String>,
    _role: Option<String>,
    _project_override: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    Err(
        "`nuo thread open` is unavailable: this build was compiled without the `interactive` feature"
            .into(),
    )
}

pub async fn run(
    action: ThreadAction,
    project_override: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let explicit_project = project_override.is_some();
    let raw_root = project_override
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let project_root = nuo_host::paths::find_project_root(&raw_root);

    match action {
        ThreadAction::Open { id, role } => open_thread(id, role, project_override).await,
        ThreadAction::List {
            json,
            hosted,
            role,
            all_workspaces,
            all_roles,
        } => {
            let Some(info) = client::discover(&project_root).await else {
                if json {
                    println!("[]");
                } else {
                    eprintln!(
                        "nuo: no server is running. Start one with `nuo` or `nuo server start`."
                    );
                }
                return Ok(());
            };
            if !client::versions_compatible(&info) {
                return Err(client::incompatibility_error(&info).into());
            }
            let filter = resolve_scope_filter(
                &project_root,
                role.as_deref(),
                all_workspaces,
                all_roles,
                explicit_project,
            )?;
            let mut rows = client::list_threads(&info, filter.clone()).await?;
            if hosted {
                rows.retain(|row| row.hosting.is_hosted());
            }
            render_threads(&rows, json, &filter);
            Ok(())
        }
        ThreadAction::Stop {
            id,
            all,
            force,
            role,
            all_workspaces,
            all_roles,
        } => {
            let info = discover_for_management(&project_root).await?;
            if all {
                let filter = resolve_scope_filter(
                    &project_root,
                    role.as_deref(),
                    all_workspaces,
                    all_roles,
                    explicit_project,
                )?;
                let rows = client::list_threads(&info, filter.clone()).await?;
                let ids: Vec<String> = rows
                    .iter()
                    .filter(|row| row.hosting.is_hosted())
                    .map(|row| row.id.clone())
                    .collect();
                if ids.is_empty() {
                    eprintln!("nuo: no hosted threads to stop in this scope.");
                    return Ok(());
                }
                if !force && !confirm("stop", ids.len(), ids.len(), &filter)? {
                    eprintln!("nuo: aborting stop; nothing was stopped.");
                    return Ok(());
                }
                let report = client::stop_threads(&info, Some(filter), ids).await?;
                report_removal("Stopped", &report)?;
                return Ok(());
            }
            let tid = id.ok_or_else(|| "thread stop needs a thread id or --all".to_string())?;
            let guard = role
                .as_deref()
                .map(|role| {
                    resolve_scope_filter(&project_root, Some(role), false, false, explicit_project)
                })
                .transpose()?;
            let report = client::stop_threads(&info, guard, vec![tid]).await?;
            report_removal("Stopped", &report)
        }
        ThreadAction::Delete {
            id,
            all,
            force,
            dry_run,
            role,
            all_workspaces,
            all_roles,
        } => {
            let info = discover_for_management(&project_root).await?;
            if all {
                let filter = resolve_scope_filter(
                    &project_root,
                    role.as_deref(),
                    all_workspaces,
                    all_roles,
                    explicit_project,
                )?;
                let rows = client::list_threads(&info, filter.clone()).await?;
                if dry_run {
                    print_purge_preview(&rows, &filter);
                    return Ok(());
                }
                if rows.is_empty() {
                    eprintln!("nuo: no thread history to remove in this scope.");
                    return Ok(());
                }
                let hosted = rows.iter().filter(|row| row.hosting.is_hosted()).count();
                if !force && !confirm("delete", rows.len(), hosted, &filter)? {
                    eprintln!("nuo: aborting purge; nothing was removed.");
                    return Ok(());
                }
                let ids = rows.into_iter().map(|row| row.id).collect();
                let report = client::purge_threads(&info, Some(filter), ids).await?;
                report_removal("Removed", &report)?;
                return Ok(());
            }
            let tid = id.ok_or_else(|| "thread rm needs a thread id or --all".to_string())?;
            let guard = role
                .as_deref()
                .map(|role| {
                    resolve_scope_filter(&project_root, Some(role), false, false, explicit_project)
                })
                .transpose()?;
            let report = client::purge_threads(&info, guard, vec![tid]).await?;
            report_removal("Removed", &report)
        }
    }
}

async fn discover_for_management(
    project_root: &std::path::Path,
) -> Result<client::ServerInfo, Box<dyn std::error::Error>> {
    let info = client::discover(project_root).await.ok_or_else(|| {
        "no server is running. Start or discover one before managing threads.".to_string()
    })?;
    if !client::versions_compatible(&info) {
        return Err(client::incompatibility_error(&info).into());
    }
    Ok(info)
}

/// Resolve the role-first scope from CLI flags. The default role is
/// `developer`, whose workspace policy binds it to the current project.
fn resolve_scope_filter(
    project_root: &std::path::Path,
    role: Option<&str>,
    all_workspaces: bool,
    all_roles: bool,
    explicit_project: bool,
) -> Result<ThreadFilter, Box<dyn std::error::Error>> {
    if all_roles && role.is_some() {
        return Err("--role and --all-roles are mutually exclusive".into());
    }
    if all_roles {
        return Ok(ThreadFilter {
            role: None,
            workspace: if all_workspaces {
                WorkspaceFilter::Any
            } else {
                WorkspaceFilter::Path(project_root.to_path_buf())
            },
        });
    }
    let scope = nuo_persistence::roles::resolve_thread_scope(role, Some(project_root))?;
    if scope.workspace.is_none() && explicit_project {
        return Err(format!(
            "role '{}' is workspace-free; --project is not applicable",
            scope.role
        )
        .into());
    }
    Ok(if all_workspaces {
        ThreadFilter {
            role: Some(scope.role),
            workspace: WorkspaceFilter::Any,
        }
    } else {
        scope.filter()
    })
}

/// Affirmative confirmation for a destructive scope action. Defaults to
/// "no"; only an exact "yes"/"y" proceeds.
fn confirm(
    verb: &str,
    count: usize,
    hosted: usize,
    filter: &ThreadFilter,
) -> Result<bool, Box<dyn std::error::Error>> {
    use std::io::BufRead;
    eprint!(
        "{} {} thread(s) ({} hosted) in {}? This cannot be undone [y/N] ",
        if verb == "delete" { "Delete" } else { "Stop" },
        count,
        hosted,
        scope_label(filter)
    );
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let reply = line.trim().to_ascii_lowercase();
    Ok(reply == "y" || reply == "yes")
}

fn scope_label(filter: &ThreadFilter) -> String {
    let role = filter.role.as_deref().unwrap_or("all roles");
    let workspace = match &filter.workspace {
        WorkspaceFilter::Any => "all workspaces".to_string(),
        WorkspaceFilter::Path(path) => path.display().to_string(),
        WorkspaceFilter::Unbound => "workspace-free".to_string(),
    };
    format!("role {role} · workspace {workspace}")
}

fn print_purge_preview(rows: &[MonitoredThread], filter: &ThreadFilter) {
    let hosted = rows.iter().filter(|row| row.hosting.is_hosted()).count();
    println!("Scope:     {}", scope_label(filter));
    println!(
        "Threads:   {} ({} hosted, {} dormant)",
        rows.len(),
        hosted,
        rows.len() - hosted
    );
    println!("Subagents: deleted with their parent threads; not counted in the user-facing total.");
}

fn report_removal(
    verb: &str,
    report: &ThreadRemovalReport,
) -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "{verb} {} thread(s); {} subagent run(s); {} already gone.",
        report.removed, report.subagents_removed, report.already_gone
    );
    for skip in &report.skipped {
        eprintln!("nuo: skipped thread '{}': {}", skip.thread_id, skip.reason);
    }
    if report.skipped.is_empty() {
        Ok(())
    } else {
        Err("some threads could not be processed".into())
    }
}

fn render_threads(sessions: &[MonitoredThread], json: bool, filter: &ThreadFilter) {
    if json {
        println!(
            "{}",
            serde_json::to_string(sessions).unwrap_or_else(|_| "[]".to_string())
        );
        return;
    }
    println!("{}", format_threads_table(sessions, filter));
}

/// One headless row: a live session plus the lineage geometry it paints.
struct ThreadRow<'a> {
    row: &'a MonitoredThread,
    /// `""` at the root lane; otherwise ancestor columns plus an elbow — the
    /// same grammar the TUI picker paints, so a terminal without the TUI still
    /// sees the tree (ADR-0120). The elbow closes on the last row of a sibling
    /// set, which is the *oldest* one (ADR-0128).
    lane: String,
}

/// Order the flat monitor snapshot into a lineage forest, without dropping a
/// row. `nuo thread ls` and the TUI picker must agree on what a branch is, so
/// this derives geometry from the same persisted facts and the same two rules:
/// newest creation first within a sibling set, and every row emitted exactly
/// once (ADR-0120, ADR-0128).
fn thread_rows(sessions: &[MonitoredThread]) -> Vec<ThreadRow<'_>> {
    use std::collections::{HashMap, HashSet};

    let present: HashSet<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
    let mut children: HashMap<&str, Vec<&MonitoredThread>> = HashMap::new();
    let mut roots: Vec<&MonitoredThread> = Vec::new();
    for row in sessions {
        match row.parent_id.as_deref() {
            Some(parent) if present.contains(parent) && parent != row.id => {
                children.entry(parent).or_default().push(row);
            }
            _ => roots.push(row),
        }
    }

    // Newest creation first, id as the tiebreak so equal timestamps still form
    // a total order. `updated_at` is a liveness signal: ordering on it would
    // move a row every time a model streams (`[INV-LINEAGE-03]`).
    let by_creation = |a: &&MonitoredThread, b: &&MonitoredThread| {
        (b.created_at, a.id.as_str()).cmp(&(a.created_at, b.id.as_str()))
    };
    roots.sort_by(by_creation);
    for list in children.values_mut() {
        list.sort_by(by_creation);
    }

    let mut out = Vec::new();
    let mut visited: HashSet<&str> = HashSet::new();
    // Explicit stack, not recursion: a long chain must not overflow the stack
    // of a CLI that a script may point at an arbitrary store.
    let mut stack: Vec<(&MonitoredThread, usize, bool, Vec<bool>)> = roots
        .iter()
        .enumerate()
        .rev()
        .map(|(index, row)| (*row, 0usize, index + 1 == roots.len(), Vec::new()))
        .collect();

    while let Some((row, depth, is_last, ancestry)) = stack.pop() {
        if !visited.insert(row.id.as_str()) {
            continue;
        }
        let mut lane = String::new();
        for (i, continues) in ancestry.iter().enumerate() {
            if i + 1 == ancestry.len() {
                lane.push_str(if is_last { "└─ " } else { "├─ " });
            } else {
                lane.push_str(if *continues { "│  " } else { "   " });
            }
        }
        let _ = depth;
        out.push(ThreadRow { row, lane });

        if let Some(kids) = children.get(row.id.as_str()) {
            let last_index = kids.len().saturating_sub(1);
            for (index, kid) in kids.iter().enumerate().rev() {
                let mut next = ancestry.clone();
                next.push(if depth == 0 { true } else { !is_last });
                stack.push((kid, depth + 1, index == last_index, next));
            }
        }
    }

    // Rows unreachable from any root (a `parent_id` cycle) still get listed.
    // They keep the same total order as every other sibling set rather than
    // whatever sequence the monitor snapshot happened to arrive in.
    let mut stranded: Vec<&MonitoredThread> = sessions
        .iter()
        .filter(|row| !visited.contains(row.id.as_str()))
        .collect();
    stranded.sort_by(by_creation);
    for row in stranded {
        out.push(ThreadRow {
            row,
            lane: String::new(),
        });
    }

    out
}

pub fn format_threads_table(sessions: &[MonitoredThread], filter: &ThreadFilter) -> String {
    let mut out = String::new();
    if sessions.is_empty() {
        out.push_str(&format!(
            "  (no thread history in {})\n",
            scope_label(filter)
        ));
        return out;
    }

    out.push_str(&format!(
        "  {:<10} {:<12} {:<24} {:<9} {:>8} {:>7} {:<10} {}\n",
        "THREAD ID", "ROLE", "WORKSPACE", "STATUS", "ROUNDS", "TOKENS", "UPDATED", "TITLE"
    ));
    for ThreadRow { row, lane } in thread_rows(sessions) {
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
        let status = if row.hosting.is_hosted() {
            row.status.as_str().to_string()
        } else {
            "dormant".to_string()
        };
        let workspace = if row.project_root.is_empty() || row.project_root == "workspace-free" {
            "–".to_string()
        } else {
            row.project_root.clone()
        };
        // Title carries the lineage: the lane geometry, the fork address when
        // the store recorded one, and a warning when the origin is not here.
        let mut title = String::new();
        if !lane.is_empty() {
            title.push_str(&lane);
        }
        match (row.fork_kind, row.fork_point) {
            (nuo_protocol::ThreadForkKind::Fork, Some(point)) => {
                title.push_str(&format!("⑂{point} "))
            }
            (nuo_protocol::ThreadForkKind::Fork, None) => title.push_str("⑂ "),
            _ => {}
        }
        if let (nuo_protocol::ThreadForkKind::Fork, Some(parent)) =
            (row.fork_kind, row.parent_id.as_deref())
            && lane.is_empty()
        {
            // An anchorless branch: the parent is not in this listing. Say so
            // instead of letting the row read as an independent thread.
            title.push_str(&format!("(from {}) ", short_id(parent)));
        }
        title.push_str(&truncate(&row.overview, 50));

        out.push_str(&format!(
            "  {:<10} {:<12} {:<24} {:<9} {:>8} {:>7} {:<10} {}\n",
            short_id(&row.id),
            truncate(&row.role, 12),
            truncate(&workspace, 24),
            status,
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
    use nuo_protocol::{ThreadForkKind, ThreadHosting, ThreadStatus};

    fn test_filter() -> ThreadFilter {
        ThreadFilter {
            role: Some("developer".into()),
            workspace: WorkspaceFilter::Path("/data".into()),
        }
    }

    #[test]
    fn scope_flags_are_role_first() {
        let project = std::path::Path::new("/repo/a");
        let developer = resolve_scope_filter(project, None, false, false, false).unwrap();
        assert_eq!(developer.role.as_deref(), Some("developer"));
        assert_eq!(
            developer.workspace,
            WorkspaceFilter::Path(project.to_path_buf())
        );

        let philosophist =
            resolve_scope_filter(project, Some("philosophist"), false, false, false).unwrap();
        assert_eq!(philosophist.role.as_deref(), Some("philosophist"));
        assert_eq!(philosophist.workspace, WorkspaceFilter::Unbound);

        assert!(
            resolve_scope_filter(project, Some("philosophist"), false, false, true).is_err(),
            "an explicit --project on a workspace-free role must be refused"
        );
        assert!(resolve_scope_filter(project, Some("no-such-role"), false, false, false).is_err());
    }

    #[test]
    fn format_threads_table_renders_header_and_rows() {
        let rows = vec![MonitoredThread {
            id: "thr_99998888".into(),
            title: Some("implement universal snapshot".into()),
            overview: "implement universal snapshot".into(),
            created_at: 100,
            updated_at: 100,
            message_count: 5,
            hosting: ThreadHosting::Hosted,
            status: ThreadStatus::Running,
            round: 2,
            turn: Some(1),
            output_tokens: 500,
            elapsed_ms: 10_000,
            current_tool: None,
            activity: None,
            context_tokens: Some(12_500),
            note: None,
            project_root: "/data".into(),
            role: "developer".into(),
            parent_id: None,
            fork_kind: ThreadForkKind::default(),
            fork_point: None,
            digest: None,
        }];
        let formatted = format_threads_table(&rows, &test_filter());
        assert!(formatted.contains("THREAD ID"));
        assert!(formatted.contains("thr_999988"));
        assert!(formatted.contains("running"));
        assert!(formatted.contains("2 › 1"));
        assert!(formatted.contains("12.5k"));
        assert!(formatted.contains("implement universal snapshot"));
    }

    /// A headless listing must show the same lineage the TUI paints: a branch
    /// is never a bare row, and its fork address comes from the store
    /// (`[INV-LINEAGE-01]`, `[INV-LINEAGE-02]`).
    #[test]
    fn threads_table_nests_branches_under_their_trunk() {
        let row = |id: &str, parent: Option<&str>, created: u64| MonitoredThread {
            id: id.into(),
            title: Some(id.into()),
            overview: id.into(),
            created_at: created,
            updated_at: created,
            message_count: 1,
            hosting: ThreadHosting::Hosted,
            status: ThreadStatus::Idle,
            round: 0,
            turn: None,
            output_tokens: 0,
            elapsed_ms: 0,
            current_tool: None,
            activity: None,
            context_tokens: None,
            note: None,
            project_root: "/data".into(),
            role: "developer".into(),
            parent_id: parent.map(str::to_string),
            fork_kind: if parent.is_some() {
                ThreadForkKind::Fork
            } else {
                ThreadForkKind::Trunk
            },
            fork_point: parent.map(|_| nuo_protocol::ForkPoint::at_turn(1, 0)),
            digest: None,
        };

        let rows = vec![
            row("trunk0001", None, 10),
            row("fork00001", Some("trunk0001"), 20),
            row("fork00002", Some("trunk0001"), 30),
        ];
        let formatted = format_threads_table(&rows, &test_filter());

        assert!(formatted.contains("trunk0001"), "{formatted}");
        assert!(
            formatted.contains("├─ ⑂r1.0 fork00002"),
            "the newest branch is not last in its sibling set, so it keeps the lane open: {formatted}"
        );
        assert!(
            formatted.contains("└─ ⑂r1.0 fork00001"),
            "the oldest branch closes its elbow: {formatted}"
        );
        // Newest creation first, and never snapshot order.
        let first = formatted.find("fork00002").unwrap();
        let second = formatted.find("fork00001").unwrap();
        assert!(
            first < second,
            "siblings are ordered by creation, newest first"
        );
    }

    /// Root rows lead with the newest thread and descend to the oldest, while a
    /// branch keeps its trunk's position however much newer it is — the same
    /// per-sibling-set rule the TUI picker applies (ADR-0128).
    #[test]
    fn threads_table_lists_roots_newest_first_without_orphaning_branches() {
        let row = |id: &str, parent: Option<&str>, created: u64| MonitoredThread {
            id: id.into(),
            title: Some(id.into()),
            overview: id.into(),
            created_at: created,
            updated_at: created,
            message_count: 1,
            hosting: ThreadHosting::Hosted,
            status: ThreadStatus::Idle,
            round: 0,
            turn: None,
            output_tokens: 0,
            elapsed_ms: 0,
            current_tool: None,
            activity: None,
            context_tokens: None,
            note: None,
            project_root: "/data".into(),
            role: "developer".into(),
            parent_id: parent.map(str::to_string),
            fork_kind: if parent.is_some() {
                ThreadForkKind::Fork
            } else {
                ThreadForkKind::Trunk
            },
            fork_point: parent.map(|_| nuo_protocol::ForkPoint::at_round(1)),
            digest: None,
        };

        let rows = vec![
            row("trunk-old1", None, 10),
            row("trunk-mid1", None, 20),
            // Created after every trunk: still nested, never promoted.
            row("fork-old1x", Some("trunk-old1"), 30),
        ];
        let formatted = format_threads_table(&rows, &test_filter());

        let pos = |needle: &str| formatted.find(needle).unwrap();
        assert!(
            pos("trunk-mid1") < pos("trunk-old1"),
            "roots are newest first: {formatted}"
        );
        assert!(
            pos("trunk-old1") < pos("fork-old1x"),
            "the newer branch stays under its older trunk: {formatted}"
        );
        assert!(
            formatted.contains("└─ ⑂r1 fork-old1x"),
            "a single branch closes its elbow: {formatted}"
        );
    }

    /// A branch whose parent is not in the listing (the common case: the parent
    /// is the thread you are attached to, or it was reaped) must announce its
    /// origin instead of masquerading as a top-level conversation.
    #[test]
    fn threads_table_marks_a_branch_whose_origin_is_absent() {
        let rows = vec![MonitoredThread {
            id: "orphan0001".into(),
            title: None,
            overview: "orphan".into(),
            created_at: 10,
            updated_at: 10,
            message_count: 1,
            hosting: ThreadHosting::Hosted,
            status: ThreadStatus::Idle,
            round: 0,
            turn: None,
            output_tokens: 0,
            elapsed_ms: 0,
            current_tool: None,
            activity: None,
            context_tokens: None,
            note: None,
            project_root: "/data".into(),
            role: "developer".into(),
            parent_id: Some("missing-parent-id".into()),
            fork_kind: ThreadForkKind::Fork,
            fork_point: None,
            digest: None,
        }];
        let formatted = format_threads_table(&rows, &test_filter());
        assert!(
            formatted.contains("(from missing-pa)"),
            "the absent origin is named (short id), not hidden: {formatted}"
        );
        assert!(
            formatted.contains("⑂"),
            "the row still reads as a branch: {formatted}"
        );
    }

    #[test]
    fn empty_threads_table_gives_actionable_tip() {
        let formatted = format_threads_table(&[], &test_filter());
        assert!(formatted.contains("no thread history"));
        assert!(formatted.contains("role developer"));
    }
}
