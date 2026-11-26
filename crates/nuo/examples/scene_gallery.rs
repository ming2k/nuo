//! Production-Path Scene Gallery (ADR-0259 `[INV-TUI-PREVIEW-02]`).
//!
//! Renders six canonical full-screen Nuo scenes directly through the production
//! shell renderers (`draw_transcript`, `draw_model_bar`, `draw_composer_with_options`,
//! `draw_diff_scene`, `draw_dashboard`, `draw_settings_view`, `draw_permission_sheet`,
//! `AskUser::render_with_tree`) with zero parallel component abstractions and zero
//! release-binary footprint.
//!
//! ## Usage
//!
//! Interactive live TTY workbench (60 ms animation loop):
//! ```text
//! cargo run -p nuo --example scene_gallery
//! ```
//!
//! Headless ANSI-truecolor snapshot dump (great for side-by-side color review):
//! ```text
//! cargo run -p nuo --example scene_gallery -- --dump --theme zen --scene 1
//! cargo run -p nuo --example scene_gallery -- --dump --theme all --scene 1 --width 100 --height 28
//! cargo run -p nuo --example scene_gallery -- --dump --plain --scene all
//! ```
//!
//! ## Interactive keys
//! - `1`..`8` / `←` `→` : switch scene
//! - `t` / `T`          : next / previous built-in theme
//! - `v`                : cycle simulated viewport (`Full`, `120×34`, `80×24`, `60×20`)
//! - `Space`            : pause / resume breathing animation
//! - `q` / `Esc` / `^C` : quit

use std::collections::HashSet;
use std::io::{self, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use graticule::{
    Backend, Block, CellWidth, Color, Event, Frame, InputReader, KeyCode, KeyEventKind,
    KeyModifiers, Line, Modifier, MouseMode, Paragraph, Rect, Span, Style, TerminalProfile,
    TerminalSession, TestTerminal, live_terminal, session_config_for_profile,
};
use nuo::shell::composer::{
    ComposerDrawOptions, ComposerProps, ComposerText, draw_composer_with_options,
};
use nuo::shell::composer_hints::ComposerHints;
use nuo::shell::keymap::Key;
use nuo::shell::overlays::ask_user::{AskUser, AskUserProps, AskUserState};
use nuo::shell::overlays::dashboard::{
    ConsoleLine, DashboardFocus, DashboardProps, draw_dashboard,
};
use nuo::shell::overlays::diff_viewer::{
    DiffFileEntry, DiffSceneProps, DiffViewerState, build_diff_lines, draw_diff_scene,
};
use nuo::shell::overlays::permission::draw_permission_sheet;
use nuo::shell::overlays::session::{SessionsModalProps, draw_sessions_modal};
use nuo::shell::overlays::toast::draw_notice_toast;
use nuo::shell::render::{
    EmptyStateGuidance, FocusPlane, ModelBarKeys, ModelBarProps, QueueBarProps, QueueItemProps,
    TasksBarProps, TranscriptProps, draw_model_bar, draw_transcript, recess_backdrop,
};
use nuo::shell::ui::ComponentTree;
use protocol::{
    MonitoredThread, NoticeSeverity, ThreadActivity, ThreadHosting, ThreadOrigin, ThreadOverview,
};
use tool::{
    PermissionRequest, Role, ToolOutput, UserQuestion, UserQuestionOption, UserQuestionRequest,
};
use ui_primitives::color_scheme::ColorSchemeConfig;
use ui_primitives::primitives::Recess;
use ui_primitives::theme::{COLOR_SCHEMES, Theme};
use view::chrome::BackgroundTaskItem;
use view::layout;
use view::model::document::{MessageKind, ToolStepStatus, TranscriptMessage};
use view::model::layout::{InteractiveTarget, InteractiveTargetKind, LayoutMap};
use view::model::selection::SelectionState;
use view::tui_config::TuiConfig;
use view::view_header::{SceneKind, TabBarItem, TabBarProps, TabStatus, ThreadContext, ViewHints};
use view::views::settings::{ConfigFocus, SettingsProps, draw_settings_view};

const SCENES: [&str; 8] = [
    "Transcript · Running",
    "Transcript · Focus & Browse",
    "Diff Scene",
    "Dashboard · Fleet",
    "Settings · Appearance",
    "Overlay · Permission Sheet",
    "Overlay · AskUser",
    "Overlay · Threads Picker [RUNNING]",
];

const VIEWPORTS: [ViewportPreset; 4] = [
    ViewportPreset::Full,
    ViewportPreset::Fixed(120, 34),
    ViewportPreset::Fixed(80, 24),
    ViewportPreset::Fixed(60, 20),
];

#[derive(Clone, Copy)]
enum ViewportPreset {
    Full,
    Fixed(u16, u16),
}

impl ViewportPreset {
    fn label(self) -> String {
        match self {
            Self::Full => "Full".to_string(),
            Self::Fixed(w, h) => format!("{w}×{h}"),
        }
    }

    fn resolve(self, outer: Rect) -> Rect {
        match self {
            Self::Full => outer,
            Self::Fixed(w, h) => Rect::new(
                outer.x,
                outer.y,
                w.min(outer.width).max(1),
                h.min(outer.height).max(1),
            ),
        }
    }
}

struct CliOptions {
    dump: bool,
    plain: bool,
    theme: Option<String>,
    scene: Option<String>,
    width: u16,
    height: u16,
}

impl Default for CliOptions {
    fn default() -> Self {
        Self {
            dump: false,
            plain: false,
            theme: None,
            scene: None,
            width: 108,
            height: 30,
        }
    }
}

fn parse_cli() -> Result<CliOptions, String> {
    let mut opts = CliOptions::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dump" => opts.dump = true,
            "--plain" => opts.plain = true,
            "--theme" => {
                opts.theme = Some(args.next().ok_or("--theme requires a value")?);
            }
            "--scene" => {
                opts.scene = Some(
                    args.next()
                        .ok_or("--scene requires a value (1..8 or all)")?,
                );
            }
            "--width" => {
                let v = args.next().ok_or("--width requires a value")?;
                opts.width = v
                    .parse::<u16>()
                    .map_err(|_| format!("invalid --width: {v}"))?;
            }
            "--height" => {
                let v = args.next().ok_or("--height requires a value")?;
                opts.height = v
                    .parse::<u16>()
                    .map_err(|_| format!("invalid --height: {v}"))?;
            }
            "--list" => {
                println!("Scenes:");
                for (idx, name) in SCENES.iter().enumerate() {
                    println!("  {}: {name}", idx + 1);
                }
                println!("\nThemes:");
                for preset in &COLOR_SCHEMES {
                    println!(
                        "  {:<12} {} — {}",
                        preset.id, preset.label, preset.description
                    );
                }
                std::process::exit(0);
            }
            "-h" | "--help" => {
                println!(
                    "Nuo Production-Path Scene Gallery (ADR-0259)\n\n\
                     Interactive:\n  cargo run -p nuo --example scene_gallery\n\n\
                     Headless dump:\n  cargo run -p nuo --example scene_gallery -- --dump \
                     [--theme <id|all>] [--scene <1..8|all>] [--width N] [--height N] [--plain]\n\n\
                     Catalog:\n  cargo run -p nuo --example scene_gallery -- --list\n\n\
                     Themes: zen, midnight, nord, catppuccin, paper, ansi16, monochrome, all\n\
                     Scenes: 1..8, all"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other} (try --help)")),
        }
    }
    Ok(opts)
}

fn resolve_theme(id: &str) -> (String, Theme) {
    let custom = ColorSchemeConfig::default();
    let norm = Theme::normalize_color_scheme(id);
    let label = Theme::color_scheme_label(&norm);
    let theme = Theme::from_color_scheme(&norm, &custom);
    (label, theme)
}

fn make_tab_items() -> Vec<TabBarItem<'static>> {
    vec![
        TabBarItem::with_status("Refactor color tokens", TabStatus::Running),
        TabBarItem::with_status("Review migration", TabStatus::Attention),
        TabBarItem::with_status("Benchmark parser", TabStatus::Succeeded),
        TabBarItem::with_status("dashboard", TabStatus::Idle),
    ]
}

fn make_running_transcript() -> Vec<TranscriptMessage> {
    let ts = 1_763_650_320_000_u64;

    let user = TranscriptMessage::new(
        Role::User,
        "Audit the running-state hues across all built-in themes and verify contrast.",
    )
    .with_round(1)
    .with_sent_at_ms(ts);

    let assistant = TranscriptMessage::new(
        Role::Assistant,
        "Updated every built-in theme to a palette-native, WCAG-AA legible running hue.",
    )
    .with_attribution("anthropic", "claude-sonnet-4-5")
    .with_effort(Some("high"))
    .with_round(1)
    .with_turn(1)
    .with_sent_at_ms(ts + 4_000);

    let mut edit_ok = TranscriptMessage::tool_step(
        "call-edit-2",
        "edit_text",
        r#"{"path":"crates/ui-primitives/src/theme/mod.rs"}"#,
    )
    .with_round(1)
    .with_turn(1)
    .with_sent_at_ms(ts + 4_000);
    edit_ok.finish_tool_step(
        "call-edit-2",
        "Updated running_fg tokens across 5 palettes",
        ToolOutput::text("Updated running_fg tokens across 5 palettes"),
        94,
    );

    let mut denied = TranscriptMessage::tool_step(
        "call-deny-3",
        "execute_command",
        r#"{"command":"git push --force"}"#,
    )
    .with_round(1)
    .with_turn(1)
    .with_sent_at_ms(ts + 4_000);
    if let MessageKind::ToolStep {
        status,
        duration_ms,
        ..
    } = &mut denied.kind
    {
        *status = ToolStepStatus::Denied;
        *duration_ms = Some(0);
    }

    let mut failed = TranscriptMessage::tool_step(
        "call-fail-4",
        "execute_command",
        r#"{"command":"cargo test --offline"}"#,
    )
    .with_round(1)
    .with_turn(1)
    .with_sent_at_ms(ts + 4_000);
    if let MessageKind::ToolStep {
        status,
        duration_ms,
        ..
    } = &mut failed.kind
    {
        *status = ToolStepStatus::Failed;
        *duration_ms = Some(420);
    }

    let mut cancelled =
        TranscriptMessage::tool_step("call-cancel-5", "search_text", r#"{"query":"RUNNING_HUE"}"#)
            .with_round(1)
            .with_turn(1)
            .with_sent_at_ms(ts + 4_000);
    cancelled.cancel_tool_step("call-cancel-5");

    let executing = TranscriptMessage::tool_step(
        "call-run-6",
        "execute_command",
        r#"{"command":"cargo nextest run -p ui-primitives"}"#,
    )
    .with_round(1)
    .with_turn(2)
    .with_sent_at_ms(ts + 8_000);

    let mut ingesting = TranscriptMessage::tool_step_announced("call-ingest-7", "edit_text")
        .with_round(1)
        .with_turn(2)
        .with_sent_at_ms(ts + 8_000);
    ingesting.set_tool_input_bytes(248);

    vec![
        user, assistant, edit_ok, denied, failed, cancelled, executing, ingesting,
    ]
}

fn make_diff_state() -> DiffViewerState {
    let old = "\
pub const COLOR_SCHEMES: [ColorSchemePreset; 7] = [
    ColorSchemePreset::static_preset(\"zen\", \"Zen\", \"Quiet charcoal with sage accents\", false),
];
pub(crate) const RUNNING_HUE: Color = Color::Rgb(56, 222, 235);
";
    let new = "\
pub const COLOR_SCHEMES: [ColorSchemePreset; 7] = [
    ColorSchemePreset::static_preset(\"zen\", \"Zen\", \"Low-chroma charcoal with muted sage\", false),
];
// Palette-native running_fg: Zen #9A9FD1 (WCAG 7.88:1, 37% sat)
";
    let (lines, additions, deletions) = build_diff_lines(old, new);
    DiffViewerState {
        files: vec![
            DiffFileEntry {
                path: "crates/ui-primitives/src/theme/presets.rs".to_string(),
                op_label: "Modified",
                additions,
                deletions,
                reviewed: false,
                lines,
            },
            DiffFileEntry {
                path: "crates/ui-primitives/src/theme/mod.rs".to_string(),
                op_label: "Modified",
                additions: 42,
                deletions: 12,
                reviewed: true,
                lines: Vec::new(),
            },
        ],
        selected_file: 0,
        scroll: 0,
    }
}

fn make_dashboard_rows() -> Vec<MonitoredThread> {
    let now = 1_763_650_400_u64;
    let mut r1 = MonitoredThread::empty("thr-8f2a10".to_string());
    r1.title = Some("Palette-native in-flight colors".to_string());
    r1.overview = "Replace neon cyan RUNNING_HUE across themes".to_string();
    r1.created_at = now - 620;
    r1.updated_at = now;
    r1.hosting = ThreadHosting::Hosted;
    r1.status = ThreadActivity::Running;
    r1.round = 3;
    r1.turn = Some(2);
    r1.output_tokens = 18_420;
    r1.elapsed_ms = 14_200;
    r1.current_tool = Some("execute_command".to_string());
    r1.activity = Some("Running cargo nextest".to_string());
    r1.project_root = "/data/projects/nuo".to_string();

    let mut r2 = MonitoredThread::empty("thr-4c9b22".to_string());
    r2.title = Some("Schema migration 0009".to_string());
    r2.overview = "Add durable index for role filters".to_string();
    r2.created_at = now - 1800;
    r2.updated_at = now - 45;
    r2.hosting = ThreadHosting::Hosted;
    r2.status = ThreadActivity::NeedsApproval;
    r2.round = 2;
    r2.turn = Some(1);
    r2.output_tokens = 9_110;
    r2.note = Some("Awaiting approval: execute_command".to_string());
    r2.project_root = "/data/projects/nuo".to_string();

    let mut r3 = MonitoredThread::empty("thr-1e7d55".to_string());
    r3.title = Some("VT parser fuzz harness".to_string());
    r3.overview = "Stress test SGR mouse sequences".to_string();
    r3.created_at = now - 3200;
    r3.updated_at = now - 190;
    r3.hosting = ThreadHosting::Hosted;
    r3.status = ThreadActivity::Failed;
    r3.round = 4;
    r3.output_tokens = 24_800;
    r3.note = Some("Provider stream closed unexpectedly".to_string());
    r3.parent_id = Some("thr-8f2a10".to_string());
    r3.origin = ThreadOrigin::Trunk;
    r3.project_root = "/data/projects/nuo".to_string();

    let mut r4 = MonitoredThread::empty("thr-9b0c11".to_string());
    r4.title = Some("Release notes v0.0.101".to_string());
    r4.overview = "Draft changelog summary".to_string();
    r4.created_at = now - 7200;
    r4.updated_at = now - 420;
    r4.hosting = ThreadHosting::Dormant;
    r4.status = ThreadActivity::Idle;
    r4.round = 1;
    r4.output_tokens = 3_400;
    r4.project_root = "/data/projects/nuo".to_string();

    vec![r1, r2, r3, r4]
}

fn draw_scene(
    frame: &mut Frame<'_>,
    scene_idx: usize,
    theme: &Theme,
    theme_id: &str,
    spinner_phase: usize,
    round_started_at: Instant,
) {
    let tab_items = make_tab_items();
    let tab_bar = TabBarProps {
        items: &tab_items,
        active_tab: 0,
        animation_ms: (spinner_phase as u64) * 100,
        unread_faults: 1,
        help_key: Some(Key::F1),
        hover: None,
        hover_elapsed_ms: 0,
    };

    match scene_idx {
        // 1. Transcript · Running
        0 => {
            let messages = make_running_transcript();
            let selection = SelectionState::default();
            let mut layout_map = LayoutMap::new();
            let render = draw_transcript(
                frame,
                &mut layout_map,
                TranscriptProps {
                    messages: &messages,
                    scroll: 0,
                    selection: &selection,
                    cell_selection: None,
                    activity: "running",
                    backoff: None,
                    awaiting_permission: false,
                    spinner_phase,
                    input: "Check Paper light contrast as well",
                    chrome_hidden: false,
                    queue_bar: QueueBarProps {
                        items: &[],
                        blocked: false,
                        expand_key: None,
                    },
                    tasks_bar: TasksBarProps::default(),
                    interrupt_key: Some(Key::ESC),
                    subagent_bar: None,
                    page_hints: Some(ViewHints {
                        kind: SceneKind::Thread,
                        role: None,
                        context: Some("Refactor color tokens"),
                        context_warn: false,
                        breadcrumbs: None,
                        can_back: false,
                        can_forward: false,
                    }),
                    tab_bar: Some(tab_bar),
                    thread_context: Some(ThreadContext {
                        role: Some("developer"),
                        workspace: "~/projects/nuo",
                        unattended: false,
                        confined: true,
                    }),
                    round_started_at: Some(round_started_at),
                    hovered_step: None,
                    focused_entry: None,
                    focus_plane: FocusPlane::Composer,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme,
                    layout: layout::Strategy::TurnBand,
                    height_cache: None,
                },
            );
            let mut input_scroll = 0;
            if render.input_rect.height > 0 {
                draw_composer_with_options(
                    ComposerProps {
                        frame,
                        input_rect: render.input_rect,
                        theme,
                        layout_map: &mut layout_map,
                        input_scroll: &mut input_scroll,
                        selection: &selection,
                    },
                    ComposerText {
                        input: "Check Paper light contrast as well",
                        byte_cursor: 34,
                    },
                    ComposerDrawOptions {
                        focused: true,
                        show_caret: true,
                        follow_caret: true,
                        record: false,
                        image_count: 0,
                        paste_count: 0,
                        hints: ComposerHints::default(),
                        ghost_text: None,
                    },
                );
            }
            if render.hint_rect.height > 0 {
                draw_model_bar(
                    frame,
                    render.hint_rect,
                    ModelBarProps {
                        current_model: "claude-sonnet-4-5",
                        model_label: Some("Claude Sonnet 4.5"),
                        model_available: true,
                        provider_name: Some("anthropic"),
                        reasoning_effort: Some("high"),
                        context_tokens: Some(48_200),
                        context_window: 200_000,
                        cost_nano_usd: Some(142_000_000),
                    },
                    theme,
                    ModelBarKeys::default(),
                );
            }
        }
        // 2. Transcript · Focus & Browse (pane wash + gutter marker + queue/tasks)
        1 => {
            let messages = make_running_transcript();
            let selection = SelectionState::default();
            let mut layout_map = LayoutMap::new();
            let queued = [QueueItemProps {
                queued_at_ms: None,
                text: "Run architecture convergence tests next".to_string(),
                is_steer: true,
            }];
            let tasks = [BackgroundTaskItem {
                id: "bg-1".to_string(),
                label: "cargo nextest".to_string(),
                running: true,
                started_at_ms: 0,
                duration_secs: Some(9),
                success: None,
                exit_code: None,
                dismissed: false,
            }];
            let render = draw_transcript(
                frame,
                &mut layout_map,
                TranscriptProps {
                    messages: &messages,
                    scroll: 0,
                    selection: &selection,
                    cell_selection: None,
                    activity: "running",
                    backoff: None,
                    awaiting_permission: false,
                    spinner_phase,
                    input: "",
                    chrome_hidden: false,
                    queue_bar: QueueBarProps {
                        items: &queued,
                        blocked: false,
                        expand_key: Some(Key::ctrl('q')),
                    },
                    tasks_bar: TasksBarProps { tasks: &tasks },
                    interrupt_key: Some(Key::ESC),
                    subagent_bar: None,
                    page_hints: Some(ViewHints {
                        kind: SceneKind::Thread,
                        role: None,
                        context: Some("Refactor color tokens"),
                        context_warn: false,
                        breadcrumbs: None,
                        can_back: true,
                        can_forward: false,
                    }),
                    tab_bar: Some(tab_bar),
                    thread_context: Some(ThreadContext {
                        role: Some("developer"),
                        workspace: "~/projects/nuo",
                        unattended: true,
                        confined: false,
                    }),
                    round_started_at: Some(round_started_at),
                    hovered_step: Some(2),
                    focused_entry: Some(InteractiveTarget {
                        message_idx: 6,
                        block_idx: 0,
                        kind: InteractiveTargetKind::ToolStep,
                    }),
                    focus_plane: FocusPlane::Transcript,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme,
                    layout: layout::Strategy::TurnBand,
                    height_cache: None,
                },
            );
            let mut input_scroll = 0;
            if render.input_rect.height > 0 {
                draw_composer_with_options(
                    ComposerProps {
                        frame,
                        input_rect: render.input_rect,
                        theme,
                        layout_map: &mut layout_map,
                        input_scroll: &mut input_scroll,
                        selection: &selection,
                    },
                    ComposerText {
                        input: "",
                        byte_cursor: 0,
                    },
                    ComposerDrawOptions {
                        focused: false,
                        show_caret: false,
                        ..ComposerDrawOptions::default()
                    },
                );
            }
            if render.hint_rect.height > 0 {
                draw_model_bar(
                    frame,
                    render.hint_rect,
                    ModelBarProps {
                        current_model: "claude-sonnet-4-5",
                        model_label: Some("Claude Sonnet 4.5"),
                        model_available: true,
                        provider_name: Some("anthropic"),
                        reasoning_effort: Some("high"),
                        context_tokens: Some(154_000),
                        context_window: 200_000,
                        cost_nano_usd: Some(418_000_000),
                    },
                    theme,
                    ModelBarKeys::default(),
                );
            }
        }
        // 3. Diff Scene
        2 => {
            let mut diff_state = make_diff_state();
            draw_diff_scene(
                frame,
                DiffSceneProps {
                    state: Some(&mut diff_state),
                    tab_bar: Some(tab_bar),
                },
                theme,
            );
        }
        // 4. Dashboard · Fleet
        3 => {
            let rows = make_dashboard_rows();
            let mut list_scroll = 0;
            let mut detail_scroll = 0;
            let log = [ConsoleLine::Receipt {
                ok: true,
                target: Some(1),
                text: "palette-native running hue verified across 5 themes".to_string(),
            }];
            draw_dashboard(
                frame,
                DashboardProps {
                    rows: &rows,
                    selected: 0,
                    focus: DashboardFocus::List,
                    list_scroll: &mut list_scroll,
                    list_follow: false,
                    detail_scroll: &mut detail_scroll,
                    log: &log,
                    prompting: false,
                    prompt_create_new: false,
                    prompt_text: "",
                    cursor_position: 0,
                    current_session_id: "thr-8f2a10",
                    show_caret: false,
                    tab_bar: Some(TabBarProps {
                        active_tab: 3,
                        ..tab_bar
                    }),
                    filter: "",
                    filtering: false,
                    view_usage: false,
                    usage_report: None,
                    usage_loading: false,
                    usage_scroll: 0,
                },
                theme,
            );
        }
        // 5. Settings · Appearance
        4 => {
            let profile = if theme.is_monochrome() {
                TerminalProfile::dec_vt100_monochrome()
            } else {
                TerminalProfile::direct_color()
            };
            let tui_config = TuiConfig::default();
            let custom = ColorSchemeConfig::default();
            let mut cat_scroll = 0;
            let mut detail_scroll = 0;
            let detail_index = Theme::color_scheme_index(theme_id);
            let props = SettingsProps {
                category_index: 0,
                detail_index,
                hover_index: None,
                focus: ConfigFocus::Detail,
                color_scheme: theme_id,
                custom_color_scheme: &custom,
                websearch: None,
                workspace: "",
                theme_files: &[],
                category_scroll: &mut cat_scroll,
                detail_scroll: &mut detail_scroll,
                breadcrumbs: None,
                theme,
                profile: &profile,
                tui_config: &tui_config,
                tab_bar: Some(tab_bar),
            };
            draw_settings_view(frame, props);
        }
        // 6. Overlay · Permission Sheet + Toast (unoccluded)
        5 => {
            let messages = make_running_transcript();
            let selection = SelectionState::default();
            let mut layout_map = LayoutMap::new();
            let render = draw_transcript(
                frame,
                &mut layout_map,
                TranscriptProps {
                    messages: &messages,
                    scroll: 0,
                    selection: &selection,
                    cell_selection: None,
                    activity: "running",
                    backoff: None,
                    awaiting_permission: true,
                    spinner_phase,
                    input: "",
                    chrome_hidden: false,
                    queue_bar: QueueBarProps {
                        items: &[],
                        blocked: false,
                        expand_key: None,
                    },
                    tasks_bar: TasksBarProps::default(),
                    interrupt_key: Some(Key::ESC),
                    subagent_bar: None,
                    page_hints: Some(ViewHints {
                        kind: SceneKind::Thread,
                        role: None,
                        context: Some("Refactor color tokens"),
                        context_warn: true,
                        breadcrumbs: None,
                        can_back: false,
                        can_forward: false,
                    }),
                    tab_bar: Some(tab_bar),
                    thread_context: Some(ThreadContext {
                        role: Some("developer"),
                        workspace: "~/projects/nuo",
                        unattended: false,
                        confined: true,
                    }),
                    round_started_at: Some(round_started_at),
                    hovered_step: None,
                    focused_entry: None,
                    focus_plane: FocusPlane::Composer,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme,
                    layout: layout::Strategy::TurnBand,
                    height_cache: None,
                },
            );
            let mut ui = ComponentTree::default();
            ui.begin(frame.area());
            let req = PermissionRequest {
                id: "perm-1".to_string(),
                tool: "execute_command".to_string(),
                label: "Run workspace tests".to_string(),
                description: "Execute cargo nextest across ui-primitives and nuo".to_string(),
                arguments: r#"{"command":"cargo nextest run -p ui-primitives -p nuo"}"#.to_string(),
                scope: "cargo nextest *".to_string(),
                suggested_scope: Some("cargo nextest *".to_string()),
                elevation: false,
                one_off: false,
                origin: None,
                hazard: None,
                submission: None,
                preview: None,
            };
            if render.input_rect.height > 0 {
                draw_permission_sheet(
                    frame,
                    &mut ui,
                    &req,
                    0,
                    false,
                    true,
                    0,
                    1,
                    render.input_rect,
                    theme,
                    &selection,
                    &mut layout_map,
                );
            }
            draw_notice_toast(
                frame,
                "Running hue calibrated (WCAG AA+)",
                NoticeSeverity::Info,
                theme,
                None,
            );
        }
        // 7. Overlay · AskUser Modal (recessed transcript + centered question dialog)
        6 => {
            let messages = make_running_transcript();
            let selection = SelectionState::default();
            let mut layout_map = LayoutMap::new();
            let _ = draw_transcript(
                frame,
                &mut layout_map,
                TranscriptProps {
                    messages: &messages,
                    scroll: 0,
                    selection: &selection,
                    cell_selection: None,
                    activity: "running",
                    backoff: None,
                    awaiting_permission: false,
                    spinner_phase,
                    input: "",
                    chrome_hidden: false,
                    queue_bar: QueueBarProps {
                        items: &[],
                        blocked: false,
                        expand_key: None,
                    },
                    tasks_bar: TasksBarProps::default(),
                    interrupt_key: Some(Key::ESC),
                    subagent_bar: None,
                    page_hints: Some(ViewHints {
                        kind: SceneKind::Thread,
                        role: None,
                        context: Some("Refactor color tokens"),
                        context_warn: false,
                        breadcrumbs: None,
                        can_back: false,
                        can_forward: false,
                    }),
                    tab_bar: Some(tab_bar),
                    thread_context: Some(ThreadContext {
                        role: Some("developer"),
                        workspace: "~/projects/nuo",
                        unattended: false,
                        confined: true,
                    }),
                    round_started_at: Some(round_started_at),
                    hovered_step: None,
                    focused_entry: None,
                    focus_plane: FocusPlane::Composer,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme,
                    layout: layout::Strategy::TurnBand,
                    height_cache: None,
                },
            );
            recess_backdrop(frame, Recess::Dim, theme);
            let mut ui = ComponentTree::default();
            ui.begin(frame.area());
            let qreq = UserQuestionRequest {
                id: "q-1".to_string(),
                questions: vec![UserQuestion {
                    header: Some("Palette".to_string()),
                    question: "Apply palette-native running hues to all themes?".to_string(),
                    options: vec![
                        UserQuestionOption {
                            label: "Apply to all presets".to_string(),
                            description: Some(
                                "Zen, Midnight, Nord, Catppuccin, and Paper".to_string(),
                            ),
                        },
                        UserQuestionOption {
                            label: "Zen only".to_string(),
                            description: Some("Keep other presets unchanged".to_string()),
                        },
                    ],
                    multi_select: false,
                }],
                origin: None,
            };
            let ask_props = AskUserProps::new(qreq.clone(), 0);
            let ask_state = AskUserState::from_request(qreq);
            AskUser::render_with_tree(&ask_props, &ask_state, frame, &mut ui, frame.area(), theme);
        }
        // 8. Overlay · Threads Picker (`[⠋ RUNNING]` + lineage tree)
        _ => {
            frame.render_widget(
                &mut Block::default().style(Style::default().bg(theme.surface())),
                frame.area(),
            );
            let now = 1_763_650_400_u64;
            let sessions = vec![
                ThreadOverview {
                    id: "thr-8f2a10".to_string(),
                    title: Some("Palette-native in-flight colors".to_string()),
                    overview: "Replace neon cyan RUNNING_HUE across themes".to_string(),
                    created_at: now - 620,
                    updated_at: now,
                    message_count: 14,
                    active: true,
                    parent_id: None,
                    origin: ThreadOrigin::Trunk,
                    digest: None,
                },
                ThreadOverview {
                    id: "thr-1e7d55".to_string(),
                    title: Some("Nord elevated contrast check".to_string()),
                    overview: "Verify WCAG AA on panel and raised surfaces".to_string(),
                    created_at: now - 300,
                    updated_at: now - 30,
                    message_count: 6,
                    active: false,
                    parent_id: Some("thr-8f2a10".to_string()),
                    origin: ThreadOrigin::Trunk,
                    digest: None,
                },
                ThreadOverview {
                    id: "thr-4c9b22".to_string(),
                    title: Some("Schema migration 0009".to_string()),
                    overview: "Add durable index for role filters".to_string(),
                    created_at: now - 1800,
                    updated_at: now - 120,
                    message_count: 22,
                    active: false,
                    parent_id: None,
                    origin: ThreadOrigin::Trunk,
                    digest: None,
                },
            ];
            let mut expanded = HashSet::new();
            expanded.insert("thr-8f2a10".to_string());
            let mut running = HashSet::new();
            running.insert("thr-8f2a10".to_string());
            running.insert("thr-1e7d55".to_string());
            let mut scroll = 0;
            let mut info_scroll = 0;
            let selection = SelectionState::default();
            let mut layout_map = LayoutMap::new();
            draw_sessions_modal(
                frame,
                SessionsModalProps {
                    sessions: &sessions,
                    expanded_sessions: Some(&expanded),
                    selected: 0,
                    scroll: &mut scroll,
                    follow: false,
                    startup_picker: false,
                    spinner_phase,
                    session_info_detail: false,
                    session_detail: None,
                    session_info_scroll: &mut info_scroll,
                    sessions_loading: false,
                    running_sessions: Some(&running),
                },
                theme,
                &selection,
                &mut layout_map,
            );
        }
    }
}

fn draw_gallery_chrome(
    frame: &mut Frame<'_>,
    bar: Rect,
    scene_idx: usize,
    theme_label: &str,
    viewport: ViewportPreset,
    paused: bool,
    theme: &Theme,
) {
    let bg = theme.raised();
    let status = if paused { "paused" } else { "live" };
    let line = Line::from(vec![
        Span::styled(
            " GALLERY ",
            Style::default()
                .fg(theme.surface())
                .bg(theme.brand())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                " [{}/{}] {} ",
                scene_idx + 1,
                SCENES.len(),
                SCENES[scene_idx]
            ),
            Style::default()
                .fg(theme.fg())
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", Style::default().fg(theme.dim()).bg(bg)),
        Span::styled("Theme: ", Style::default().fg(theme.muted()).bg(bg)),
        Span::styled(
            theme_label.to_string(),
            Style::default()
                .fg(theme.running())
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", Style::default().fg(theme.dim()).bg(bg)),
        Span::styled(
            format!("{} ({status})", viewport.label()),
            Style::default().fg(theme.muted()).bg(bg),
        ),
        Span::styled(
            "  [1-8/←→ scene  t/T theme  v size  Space anim  q quit]",
            Style::default().fg(theme.dim()).bg(bg),
        ),
    ]);
    frame.render_widget(
        &mut Paragraph::new(line).style(Style::default().bg(bg)),
        bar,
    );
}

fn ansi_fg(c: Color, out: &mut String) {
    match c {
        Color::Reset => out.push_str("\x1b[39m"),
        Color::Black => out.push_str("\x1b[30m"),
        Color::Red => out.push_str("\x1b[31m"),
        Color::Green => out.push_str("\x1b[32m"),
        Color::Yellow => out.push_str("\x1b[33m"),
        Color::Blue => out.push_str("\x1b[34m"),
        Color::Magenta => out.push_str("\x1b[35m"),
        Color::Cyan => out.push_str("\x1b[36m"),
        Color::Gray => out.push_str("\x1b[37m"),
        Color::DarkGray => out.push_str("\x1b[90m"),
        Color::LightRed => out.push_str("\x1b[91m"),
        Color::LightGreen => out.push_str("\x1b[92m"),
        Color::LightYellow => out.push_str("\x1b[93m"),
        Color::LightBlue => out.push_str("\x1b[94m"),
        Color::LightMagenta => out.push_str("\x1b[95m"),
        Color::LightCyan => out.push_str("\x1b[96m"),
        Color::White => out.push_str("\x1b[97m"),
        Color::Indexed(i) => {
            use std::fmt::Write as _;
            let _ = write!(out, "\x1b[38;5;{i}m");
        }
        Color::Rgb(r, g, b) => {
            use std::fmt::Write as _;
            let _ = write!(out, "\x1b[38;2;{r};{g};{b}m");
        }
    }
}

fn ansi_bg(c: Color, out: &mut String) {
    match c {
        Color::Reset => out.push_str("\x1b[49m"),
        Color::Black => out.push_str("\x1b[40m"),
        Color::Red => out.push_str("\x1b[41m"),
        Color::Green => out.push_str("\x1b[42m"),
        Color::Yellow => out.push_str("\x1b[43m"),
        Color::Blue => out.push_str("\x1b[44m"),
        Color::Magenta => out.push_str("\x1b[45m"),
        Color::Cyan => out.push_str("\x1b[46m"),
        Color::Gray => out.push_str("\x1b[47m"),
        Color::DarkGray => out.push_str("\x1b[100m"),
        Color::LightRed => out.push_str("\x1b[101m"),
        Color::LightGreen => out.push_str("\x1b[102m"),
        Color::LightYellow => out.push_str("\x1b[103m"),
        Color::LightBlue => out.push_str("\x1b[104m"),
        Color::LightMagenta => out.push_str("\x1b[105m"),
        Color::LightCyan => out.push_str("\x1b[106m"),
        Color::White => out.push_str("\x1b[107m"),
        Color::Indexed(i) => {
            use std::fmt::Write as _;
            let _ = write!(out, "\x1b[48;5;{i}m");
        }
        Color::Rgb(r, g, b) => {
            use std::fmt::Write as _;
            let _ = write!(out, "\x1b[48;2;{r};{g};{b}m");
        }
    }
}

fn dump_grid(grid: &graticule::CellGrid, plain: bool) -> String {
    let (w, h) = grid.size();
    let mut out = String::with_capacity((w as usize) * (h as usize) * 4);
    for y in 0..h {
        let mut last_style: Option<Style> = None;
        for x in 0..w {
            let cell = grid.get_or_empty(x, y);
            if cell.width == CellWidth::Continuation {
                continue;
            }
            if !plain && last_style != Some(cell.style) {
                out.push_str("\x1b[0m");
                if cell.style.add.contains(Modifier::BOLD) {
                    out.push_str("\x1b[1m");
                }
                if cell.style.add.contains(Modifier::ITALIC) {
                    out.push_str("\x1b[3m");
                }
                if cell.style.add.contains(Modifier::UNDERLINE) {
                    out.push_str("\x1b[4m");
                }
                if cell.style.add.contains(Modifier::REVERSE) {
                    out.push_str("\x1b[7m");
                }
                ansi_fg(cell.style.fg, &mut out);
                ansi_bg(cell.style.bg, &mut out);
                last_style = Some(cell.style);
            }
            out.push_str(cell.symbol.as_str());
        }
        if !plain {
            out.push_str("\x1b[0m");
        }
        out.push('\n');
    }
    out
}

fn run_dump(opts: &CliOptions) -> io::Result<()> {
    let theme_ids: Vec<String> = match opts.theme.as_deref() {
        Some("all") => COLOR_SCHEMES.iter().map(|s| s.id.to_string()).collect(),
        Some(id) => vec![Theme::normalize_color_scheme(id)],
        None => vec!["zen".to_string()],
    };
    let scene_indices: Vec<usize> = match opts.scene.as_deref() {
        Some("all") => (0..SCENES.len()).collect(),
        Some(s) => {
            let n = s
                .parse::<usize>()
                .ok()
                .filter(|&v| (1..=SCENES.len()).contains(&v))
                .unwrap_or(1);
            vec![n - 1]
        }
        None => vec![0],
    };

    let mut stdout = io::stdout().lock();
    let started = Instant::now();
    for theme_id in &theme_ids {
        let (label, theme) = resolve_theme(theme_id);
        for &scene_idx in &scene_indices {
            let mut term = TestTerminal::new(opts.width, opts.height);
            term.draw(|f| {
                draw_scene(f, scene_idx, &theme, theme_id, 4, started);
            });
            writeln!(
                stdout,
                "=== Scene {}/{}: {} | Theme: {} ({}) | {}×{} ===",
                scene_idx + 1,
                SCENES.len(),
                SCENES[scene_idx],
                label,
                theme_id,
                opts.width,
                opts.height
            )?;
            write!(stdout, "{}", dump_grid(term.buffer(), opts.plain))?;
        }
    }
    Ok(())
}

fn run_interactive(opts: &CliOptions) -> io::Result<()> {
    let profile = nuo::shell::terminal::detect_terminal_profile();
    let config = session_config_for_profile(&profile, MouseMode::None);
    let _session = TerminalSession::enter(config)?;
    let backend = Backend::new(io::stdout()).with_profile(profile);
    let mut terminal = live_terminal(backend);

    let (tx, rx) = mpsc::channel();
    let _reader = InputReader::spawn_with(move |ev| {
        if let Event::Key(k) = &ev
            && k.kind == KeyEventKind::Release
        {
            return true;
        }
        tx.send(ev).is_ok()
    })?;

    let mut scene_idx = opts
        .scene
        .as_deref()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| (1..=SCENES.len()).contains(&n))
        .map(|n| n - 1)
        .unwrap_or(0);
    let mut theme_idx = opts
        .theme
        .as_deref()
        .map(Theme::color_scheme_index)
        .unwrap_or(0);
    let mut viewport_idx = 0usize;
    let mut paused = false;
    let mut spinner_phase = 0usize;
    let started = Instant::now();

    loop {
        let theme_id = COLOR_SCHEMES[theme_idx].id.clone();
        let (theme_label, theme) = resolve_theme(theme_id.as_ref());
        let viewport = VIEWPORTS[viewport_idx];

        terminal.draw(|frame| {
            let full = frame.area();
            frame.render_widget(
                &mut Block::default().style(Style::default().bg(theme.surface())),
                full,
            );
            if full.height < 2 {
                return;
            }
            let content_outer = Rect::new(full.x, full.y, full.width, full.height - 1);
            let bar = Rect::new(full.x, full.y + full.height - 1, full.width, 1);
            let scene_area = viewport.resolve(content_outer);
            let mut scratch = TestTerminal::new(scene_area.width, scene_area.height);
            scratch.draw(|sf| {
                draw_scene(
                    sf,
                    scene_idx,
                    &theme,
                    theme_id.as_ref(),
                    spinner_phase,
                    started,
                );
            });
            blit_grid(frame, scene_area, scratch.buffer());
            draw_gallery_chrome(
                frame,
                bar,
                scene_idx,
                &theme_label,
                viewport,
                paused,
                &theme,
            );
        })?;

        match rx.recv_timeout(Duration::from_millis(60)) {
            Ok(Event::Key(k)) => {
                let ctrl_c =
                    k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL);
                if ctrl_c || k.code == KeyCode::Esc || k.code == KeyCode::Char('q') {
                    break;
                }
                match k.code {
                    KeyCode::Char(c @ '1'..='8') => {
                        scene_idx = (c as usize) - ('1' as usize);
                    }
                    KeyCode::Right | KeyCode::Char('l') => {
                        scene_idx = (scene_idx + 1) % SCENES.len();
                    }
                    KeyCode::Left | KeyCode::Char('h') => {
                        scene_idx = (scene_idx + SCENES.len() - 1) % SCENES.len();
                    }
                    KeyCode::Char('t') => {
                        theme_idx = (theme_idx + 1) % COLOR_SCHEMES.len();
                    }
                    KeyCode::Char('T') => {
                        theme_idx = (theme_idx + COLOR_SCHEMES.len() - 1) % COLOR_SCHEMES.len();
                    }
                    KeyCode::Char('v') => {
                        viewport_idx = (viewport_idx + 1) % VIEWPORTS.len();
                    }
                    KeyCode::Char(' ') => {
                        paused = !paused;
                    }
                    _ => {}
                }
            }
            Ok(Event::Resize(_, _)) => {}
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if !paused {
                    spinner_phase = spinner_phase.wrapping_add(1);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    Ok(())
}

fn blit_grid(frame: &mut Frame<'_>, dest: Rect, src: &graticule::CellGrid) {
    let (sw, sh) = src.size();
    frame.paint(dest, |paint| {
        for y in 0..sh.min(dest.height) {
            for x in 0..sw.min(dest.width) {
                if let Some(cell) = src.get(x, y) {
                    paint.set(dest.x + x, dest.y + y, cell.clone());
                }
            }
        }
    });
}

fn main() {
    let opts = match parse_cli() {
        Ok(o) => o,
        Err(msg) => {
            eprintln!("error: {msg}");
            std::process::exit(2);
        }
    };
    let result = if opts.dump {
        run_dump(&opts)
    } else {
        run_interactive(&opts)
    };
    if let Err(err) = result {
        eprintln!("scene_gallery error: {err}");
        std::process::exit(1);
    }
}
