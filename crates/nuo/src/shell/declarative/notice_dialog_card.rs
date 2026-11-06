//! Pure Declarative Notice Center Dialog Component (ADR-0072 / ADR-0073 / ADR-0112 / ADR-0134 / ADR-0146).
//!
//! Provides isolated visual inspection and interactive testing for the sovereign
//! client-global notice center modal dialog (`DialogKind::Notices`, ADR-0134), validating:
//! - Multi-card vertical scrolling and selection anchoring (`[INV-SURFACE-02]`).
//! - Severity band color representation without glyph/text badges (ADR-0139).
//! - Expandable technical diagnostic detail tree with box-drawing gutters.
//! - Responsive bounds centering within `ContentModalSpec::TOOLS`.
//! - Empty state posture and footer shortcut affordances (`Alt-o`, `↑/↓`, `Enter`, `c`, `x`, `Esc`).

use super::{
    Component,
    story::{Story, StorybookComponent},
};
use crate::shell::declarative::key::Key;
use crate::shell::overlays::draw_notices_modal;
use crate::shell::surfaces::NoticesDialog;
use nuo_ui_primitives::theme::Theme;
use nuo_view::model::log::{LineStatus, SurfaceLine};
use nuotc::{Frame, Rect};

/// Presentation properties for the Notice Center modal dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoticeDialogCardProps {
    pub lines: Vec<SurfaceLine>,
}

impl NoticeDialogCardProps {
    pub fn new(lines: Vec<SurfaceLine>) -> Self {
        Self { lines }
    }

    /// Canonical domain extraction bridge from a slice of surface log lines (ADR-0112 `[INV-TUI-TIER-02]`).
    pub fn from_lines(lines: &[SurfaceLine]) -> Self {
        Self {
            lines: lines.to_vec(),
        }
    }
}

/// Volatile interactive state for Notice Center dialog inspection.
#[derive(Clone, Debug)]
pub struct NoticeDialogCardState {
    pub dialog: NoticesDialog,
    pub cleared: bool,
}

impl Default for NoticeDialogCardState {
    fn default() -> Self {
        Self {
            dialog: NoticesDialog {
                index: 0,
                scroll: 0,
                follow: true,
                keys_open: false,
                keys_scroll: 0,
                detail: false,
                body_height: 12,
            },
            cleared: false,
        }
    }
}

impl NoticeDialogCardState {
    pub fn new(index: usize, detail: bool) -> Self {
        Self {
            dialog: NoticesDialog {
                index,
                scroll: 0,
                follow: true,
                keys_open: false,
                keys_scroll: 0,
                detail,
                body_height: 12,
            },
            cleared: false,
        }
    }
}

/// Interactive messages for notice center storybook inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeDialogCardMsg {
    SelectPrev,
    SelectNext,
    ToggleDetail,
    PageUp,
    PageDown,
    SelectFirst,
    SelectLast,
    Copy,
    Clear,
    Reset,
}

/// Outward command outcome emitted by notice center inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeDialogCardCmd {
    Copied,
    Cleared,
    Reset,
}

pub struct NoticeDialogCard;

impl Component for NoticeDialogCard {
    type Props = NoticeDialogCardProps;
    type State = NoticeDialogCardState;
    type Msg = NoticeDialogCardMsg;
    type Cmd = NoticeDialogCardCmd;

    fn render(
        props: &Self::Props,
        state: &Self::State,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
    ) {
        if area.width < 20 || area.height < 8 {
            return;
        }

        let lines: &[SurfaceLine] = if state.cleared { &[] } else { &props.lines };
        let mut dialog = state.dialog.clone();
        draw_notices_modal(frame, area, lines, &mut dialog, theme);
    }

    fn update(props: &Self::Props, state: &mut Self::State, msg: Self::Msg) -> Option<Self::Cmd> {
        let active_count = if state.cleared { 0 } else { props.lines.len() };
        match msg {
            NoticeDialogCardMsg::SelectPrev => {
                if active_count > 0 {
                    state.dialog.index = state.dialog.index.saturating_sub(1);
                    state.dialog.detail = false;
                    state.dialog.follow = false;
                }
                None
            }
            NoticeDialogCardMsg::SelectNext => {
                if active_count > 0 {
                    state.dialog.index = (state.dialog.index + 1).min(active_count - 1);
                    state.dialog.detail = false;
                    state.dialog.follow = false;
                }
                None
            }
            NoticeDialogCardMsg::ToggleDetail => {
                if active_count > 0 {
                    state.dialog.detail = !state.dialog.detail;
                }
                None
            }
            NoticeDialogCardMsg::PageUp => {
                let page = if state.dialog.body_height > 0 {
                    state.dialog.body_height as usize
                } else {
                    6
                };
                state.dialog.scroll = state.dialog.scroll.saturating_sub(page);
                state.dialog.follow = false;
                None
            }
            NoticeDialogCardMsg::PageDown => {
                let page = if state.dialog.body_height > 0 {
                    state.dialog.body_height as usize
                } else {
                    6
                };
                state.dialog.scroll = state.dialog.scroll.saturating_add(page);
                state.dialog.follow = false;
                None
            }
            NoticeDialogCardMsg::SelectFirst => {
                if active_count > 0 {
                    state.dialog.index = 0;
                    state.dialog.detail = false;
                    state.dialog.follow = false;
                }
                None
            }
            NoticeDialogCardMsg::SelectLast => {
                if active_count > 0 {
                    state.dialog.index = active_count - 1;
                    state.dialog.detail = false;
                    state.dialog.follow = false;
                }
                None
            }
            NoticeDialogCardMsg::Copy => Some(NoticeDialogCardCmd::Copied),
            NoticeDialogCardMsg::Clear => {
                state.cleared = true;
                state.dialog.index = 0;
                state.dialog.detail = false;
                Some(NoticeDialogCardCmd::Cleared)
            }
            NoticeDialogCardMsg::Reset => {
                state.cleared = false;
                Some(NoticeDialogCardCmd::Reset)
            }
        }
    }
}

fn massive_overflow_lines() -> Vec<SurfaceLine> {
    let topics = [
        "backend", "sandbox", "auth", "fs", "mcp", "catalog", "provider", "link",
    ];
    let mut lines = Vec::with_capacity(50);
    let base_time = 1_700_000_000_000u64;

    for i in 0..50 {
        let topic = topics[i % topics.len()];
        let at_ms = base_time + (i as u64 * 45_000);
        let status = match i % 5 {
            0 => LineStatus::Failed,
            1 => LineStatus::Warn,
            2 => LineStatus::Ok,
            _ => LineStatus::Info,
        };
        let text = match status {
            LineStatus::Failed => {
                format!("Operational fault #{i}: failure encountered in subsystem '{topic}'")
            }
            LineStatus::Warn => {
                format!("Subsystem warning #{i}: degraded capacity detected on '{topic}'")
            }
            LineStatus::Ok => {
                format!("State transition #{i}: observation recorded on domain '{topic}'")
            }
            _ => format!("Routine synchronization #{i}: heartbeat verified for '{topic}'"),
        };
        let mut line = SurfaceLine::disclosure(status, text, at_ms).with_label(topic);
        if i % 7 == 0 {
            line = line.with_detail(format!(
                "Diagnostic telemetry context for entry #{i}:\n  Subsystem: {topic}\n  Timestamp: +{}s\n  Status: {status:?}",
                i * 45
            ));
        }
        lines.push(line);
    }
    lines
}

fn extreme_overflow_lines() -> Vec<SurfaceLine> {
    vec![
        SurfaceLine::disclosure(
            LineStatus::Failed,
            "Failed to execute command outside workspace boundary: https://internal.corp.network/v2/audit/reports/storage_cluster_access_token_and_security_policy_enforcement_violation_notice_payload?id=018f3a9e-bc42-7011-80a2-f81d4b6849a1&env=production&trace_carrier_tag=corp_secops_enforce_block_2026: permission denied by kernel eBPF security filter",
            1_700_000_000_000,
        )
        .with_label("sandbox")
        .with_detail(
            "Call hierarchy:\n  0: nuo_server::sandbox::policy::enforce_path (policy.rs:241)\n  1: nuo_harness::execution::worker::dispatch (worker.rs:89)\n  2: tokio::runtime::task::core::run (runtime.rs:114)\nRoot Cause:\n  EACCES (errno 13): Permission denied - target path /etc/shadow resides strictly outside sovereign project root /data/projects/nuo"
        ),
        SurfaceLine::disclosure(
            LineStatus::Warn,
            "DNS resolution retry 4 of 5 for upstream-mirror.internal.zone.domain.test: query timed out after 5000ms; downstream tools will experience increased latency",
            1_700_000_060_000,
        )
        .with_label("network"),
    ]
}

impl StorybookComponent for NoticeDialogCard {
    fn category() -> &'static str {
        "Overlays"
    }

    fn name() -> &'static str {
        "NoticeCenterDialog"
    }

    fn stories() -> Vec<Story<Self::Props, Self::State>> {
        vec![
            Story::new(
                "Empty State (Zero Disclosures)",
                "Clean baseline log posture when no operational notices or diagnostic faults exist",
                NoticeDialogCardProps::new(vec![]),
                NoticeDialogCardState::new(0, false),
            ),
            Story::new(
                "Standard Multi-Severity History",
                "Card stack rendering Info, Warning, and Error items with topic pills, fault count, and timestamps",
                NoticeDialogCardProps::new(vec![
                    SurfaceLine::disclosure(
                        LineStatus::Info,
                        "Catalog refreshed: 18 tool specifications loaded into workspace registry",
                        1_700_000_000_000,
                    )
                    .with_label("catalog"),
                    SurfaceLine::disclosure(
                        LineStatus::Warn,
                        "Rate limit threshold reached (80%); throttling outgoing model requests",
                        1_700_000_060_000,
                    )
                    .with_label("provider"),
                    SurfaceLine::disclosure(
                        LineStatus::Failed,
                        "Upstream model endpoint connection timed out after 30s during token stream",
                        1_700_000_120_000,
                    )
                    .with_label("backend"),
                    SurfaceLine::disclosure(
                        LineStatus::Ok,
                        "Primary workspace selection synchronized to system clipboard buffer",
                        1_700_000_180_000,
                    )
                    .with_label("clipboard"),
                ]),
                NoticeDialogCardState::new(2, false),
            ),
            Story::new(
                "Expanded Technical Detail",
                "Active diagnostic card with expanded box-drawing tree showing HTTP trace and error context",
                NoticeDialogCardProps::new(vec![
                    SurfaceLine::disclosure(
                        LineStatus::Failed,
                        "MCP initialize handshake failed: connection closed by peer",
                        1_700_000_000_000,
                    )
                    .with_label("mcp")
                    .with_detail(
                        "POST /mcp/v1/initialize HTTP/1.1\nHost: 127.0.0.1:8080\nStatus: 502 Bad Gateway\nBody: {\"error\":\"daemon worker process exited prematurely with code 1\"}\nTrace: nuo_mcp::transport::stdio::recv -> handshake_failed",
                    ),
                    SurfaceLine::disclosure(
                        LineStatus::Warn,
                        "Model token quota degraded: fallback to secondary provider activated",
                        1_700_000_030_000,
                    )
                    .with_label("quotas"),
                    SurfaceLine::disclosure(
                        LineStatus::Info,
                        "Background file indexing completed: 1,420 AST nodes cached in memory",
                        1_700_000_060_000,
                    )
                    .with_label("indexer"),
                ]),
                NoticeDialogCardState::new(0, true),
            ),
            Story::new(
                "Massive History Overflow (50 Notices)",
                "Stress fixture simulating a long session with 50 operational disclosures and multiple faults",
                NoticeDialogCardProps::new(massive_overflow_lines()),
                NoticeDialogCardState::new(25, false),
            ),
            Story::new(
                "Extreme Long Message & Word Wrap",
                "Narrow bounds test with unbroken URI tokens, deep error chains, and aggressive text wrapping",
                NoticeDialogCardProps::new(extreme_overflow_lines()),
                NoticeDialogCardState::new(0, true),
            ),
        ]
    }

    fn map_key_to_msg(key: &Key, _props: &Self::Props, _state: &Self::State) -> Option<Self::Msg> {
        match key {
            Key::Up | Key::Char('k') => Some(NoticeDialogCardMsg::SelectPrev),
            Key::Down | Key::Char('j') => Some(NoticeDialogCardMsg::SelectNext),
            Key::Enter | Key::Char(' ') => Some(NoticeDialogCardMsg::ToggleDetail),
            Key::PageUp => Some(NoticeDialogCardMsg::PageUp),
            Key::PageDown => Some(NoticeDialogCardMsg::PageDown),
            Key::Home | Key::Char('g') => Some(NoticeDialogCardMsg::SelectFirst),
            Key::End | Key::Char('G') => Some(NoticeDialogCardMsg::SelectLast),
            Key::Char('c') | Key::Char('y') => Some(NoticeDialogCardMsg::Copy),
            Key::Char('x') | Key::Char('d') => Some(NoticeDialogCardMsg::Clear),
            Key::Char('r') | Key::Char('R') => Some(NoticeDialogCardMsg::Reset),
            _ => None,
        }
    }
}
