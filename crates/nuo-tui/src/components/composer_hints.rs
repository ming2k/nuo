//! Composer-native meta row: the single hint line painted inside the composer panel.
//!
//! Under the plane-less chat surface (ADR-0173):
//! - Idle: `Enter send` (right)
//! - Running: `Alt+S steer now` (left) / `Enter queue follow-up` (right)
//! - Completion: `Esc dismiss` (left) / `Tab / Enter select` (right)
//! - History: `Esc close` (left) / `Tab / Enter insert` (right)

use nuotc::{Color, Modifier, Span, Style};

use super::super::Theme;
use super::super::keymap::{HintSide, LiveHint};
use super::keycap::keycap_style;
use crate::modal_keys::live_history_hints;
use crate::session::{HintState, live_chat_hints};

// Width ladder

/// Width-degradation ladder for the keys row.
#[derive(Clone, Copy)]
pub(crate) enum ActionDensity {
    Full,
    Compact,
    Tiny,
}

impl ActionDensity {
    pub(crate) fn for_width(row_width: usize) -> Self {
        if row_width >= 50 {
            ActionDensity::Full
        } else if row_width >= 24 {
            ActionDensity::Compact
        } else {
            ActionDensity::Tiny
        }
    }

    fn compact(self) -> bool {
        matches!(self, ActionDensity::Compact | ActionDensity::Tiny)
    }
}

// Compose target

/// What the live buffer represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ComposeTarget {
    /// Plain prompt opening a new turn (idle).
    #[default]
    Prompt,
    /// Slash command buffer.
    Command,
    /// Agent running: steer or follow-up.
    Running(crate::app::ComposerSendMode),
    /// Active completion popup.
    Completion {
        kind: crate::completion::CompletionKind,
    },
    /// Inline ↑/↓ recall pointer on a history row (ADR-0192). Distinct from
    /// `HistorySearch` (the Ctrl+R modal): this is the chat-surface pointer
    /// state, not a modal, and its hint set (`Esc draft / Enter send`) rides
    /// the ordinary hint row.
    HistoryRecall,
    /// History search panel active (Ctrl+R).
    HistorySearch,
}

/// Derive the compose target from current state and active composer extension.
pub(crate) fn compose_target_for_extension(
    busy: bool,
    send_mode: Option<crate::app::ComposerSendMode>,
    is_slash: bool,
    extension: Option<crate::composer_extension::ComposerExtensionKind>,
    in_history_recall: bool,
) -> ComposeTarget {
    match extension {
        Some(crate::composer_extension::ComposerExtensionKind::HistorySearch) => {
            ComposeTarget::HistorySearch
        }
        Some(crate::composer_extension::ComposerExtensionKind::SlashCompletion) => {
            ComposeTarget::Completion {
                kind: crate::completion::CompletionKind::Slash,
            }
        }
        Some(crate::composer_extension::ComposerExtensionKind::PathCompletion) => {
            ComposeTarget::Completion {
                kind: crate::completion::CompletionKind::Path,
            }
        }
        None => {
            if in_history_recall {
                ComposeTarget::HistoryRecall
            } else if busy {
                ComposeTarget::Running(send_mode.unwrap_or_default())
            } else if is_slash {
                ComposeTarget::Command
            } else {
                ComposeTarget::Prompt
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ComposerHints {
    pub compose_target: ComposeTarget,
    pub can_retry: bool,
    /// The inline ↑/↓ recall pointer badge (`Some((position, total, edited))`,
    /// 1-based) derived from `App::history_recall_badge`. `None` while the
    /// composer shows the draft — the top chrome row then renders its
    /// ordinary breathing row. The renderer appends the `draft saved` /
    /// `edited` clauses; this field carries only the pointer facts so the
    /// derivation stays testable without a `Theme`.
    pub history_recall: Option<(usize, usize, bool)>,
    /// Whether the stashed recall draft (`App::draft_capsule`) is non-empty
    /// — the `· draft saved` reassurance clause on the badge. Kept separate
    /// from `history_recall` so a future badge variant can consume either
    /// fact independently.
    pub recall_draft_saved: bool,
    /// The effective chord for toggling send mode while running (ADR-0172):
    /// the hint row advertises exactly the binding that fires. Defaults to the
    /// canonical `Tab` when unremapped.
    pub toggle_mode_key: crate::keymap::Key,
}

impl Default for ComposerHints {
    fn default() -> Self {
        Self {
            compose_target: ComposeTarget::Prompt,
            can_retry: false,
            history_recall: None,
            recall_draft_saved: false,
            toggle_mode_key: crate::keymap::Key::TAB,
        }
    }
}

/// Build the composer's hint row separated into left and right spans.
///
/// The chord set (and its labels) come from the Conversation scene's own scheme
/// (`session::live_chat_hints`, ADR-0172): what the row advertises is exactly
/// what `resolve_chat_surface_key` handles, so a hint can never drift from a
/// dead shortcut. Only the *presentation* — which side a chord lands on, the
/// 3-col gap between nav chords, the `Tab / Enter` action pairing, per-state
/// label styling, and the `command`/`retry` branding — lives here.
pub(crate) fn hint_row_parts(
    can_retry: bool,
    density: ActionDensity,
    target: ComposeTarget,
    theme: &Theme,
    bg: Color,
    toggle_mode_key: crate::keymap::Key,
) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    let key_style = keycap_style(theme).bg(bg);
    let hint_style = theme.keycap_label_style().bg(bg);
    let compact = density.compact();
    let tiny = matches!(density, ActionDensity::Tiny);

    // HistorySearch is a modal whose keys are owned by its own scheme
    // (`modal_keys::live_history_hints`, ADR-0172); this row renders exactly
    // those chords with adaptive density and ADR-0054 visual grammar.
    if target == ComposeTarget::HistorySearch {
        let hints = live_history_hints();
        let mut left: Vec<Span<'static>> = Vec::new();
        for h in hints {
            if h.side != HintSide::Nav {
                continue;
            }
            if tiny && h.key != crate::keymap::Key::ESC {
                continue;
            }
            if compact && h.label == "navigate" {
                // Drop navigate in compact mode to preserve delete and close
                continue;
            }
            if !left.is_empty() {
                left.push(Span::styled("   ", hint_style));
            }
            left.push(Span::styled(h.display_key(), key_style));
            left.push(Span::styled(format!(" {}", h.label), hint_style));
        }
        let actions: Vec<&LiveHint> = hints
            .iter()
            .filter(|h| h.side == HintSide::Action)
            .collect();
        let mut right: Vec<Span<'static>> = Vec::new();
        let action_key_style = Style::default()
            .fg(theme.keycap_accent())
            .bg(bg)
            .add_modifier(Modifier::BOLD);
        if compact || tiny {
            if let Some(last) = actions.last() {
                right.push(Span::styled(last.display_key(), action_key_style));
                right.push(Span::styled(format!(" {}", last.label), hint_style));
            }
        } else {
            for (i, h) in actions.iter().enumerate() {
                if i > 0 {
                    right.push(Span::styled(" / ", hint_style));
                }
                right.push(Span::styled(h.display_key(), action_key_style));
            }
            if let Some(last) = actions.last() {
                right.push(Span::styled(format!(" {}", last.label), hint_style));
            }
        }
        return (left, right);
    }

    let action_key_style = match target {
        ComposeTarget::Running(crate::app::ComposerSendMode::Steer) => {
            Style::default().fg(theme.warn()).bg(bg).add_modifier(Modifier::BOLD)
        }
        ComposeTarget::Running(crate::app::ComposerSendMode::FollowUp) => {
            Style::default().fg(theme.info()).bg(bg).add_modifier(Modifier::BOLD)
        }
        _ => Style::default()
            .fg(theme.keycap_accent())
            .bg(bg)
            .add_modifier(Modifier::BOLD),
    };
    let state = match target {
        ComposeTarget::Prompt => HintState::Idle,
        ComposeTarget::Command => HintState::Command,
        ComposeTarget::HistoryRecall => HintState::Recall,
        ComposeTarget::Running(mode) => HintState::Running(mode),
        ComposeTarget::Completion { .. } => HintState::Completion,
        ComposeTarget::HistorySearch => unreachable!(),
    };
    let hints = live_chat_hints(state, toggle_mode_key);

    // Left: navigation affordances, joined by a 3-col gap. Hidden on Tiny
    // terminals for the plain prompt / command rows; the toggle verb's hint
    // (canonical Tab, remapped per ADR-0172) drops when compact so a running
    // row stays tight.
    let mut left: Vec<Span<'static>> = Vec::new();
    let hide_nav = tiny && matches!(state, HintState::Idle | HintState::Command);
    if !hide_nav {
        for h in &hints {
            if h.side != HintSide::Nav {
                continue;
            }
            if h.key == toggle_mode_key && compact {
                continue;
            }
            if !left.is_empty() {
                left.push(Span::styled("   ", hint_style));
            }
            left.push(Span::styled(h.display_key(), key_style));
            left.push(Span::styled(format!(" {}", h.label), hint_style));
        }
    }

    // Right: action affordances. Multiple keys pair as `Tab / Enter`, with the
    // verb key styled per ADR-0054 (accent on key, subdued text on label); the
    // `command` and `retry` suffixes are presentation branding.
    let mut right: Vec<Span<'static>> = Vec::new();
    let actions: Vec<&crate::keymap::LiveHint> = hints
        .iter()
        .filter(|h| h.side == HintSide::Action)
        .collect();
    for (i, h) in actions.iter().enumerate() {
        if i > 0 {
            right.push(Span::styled(" / ", hint_style));
        }
        right.push(Span::styled(h.display_key(), action_key_style));
    }
    if let Some(last) = actions.last() {
        right.push(Span::styled(format!(" {}", last.label), hint_style));
    }
    if target == ComposeTarget::Command {
        right.push(Span::styled(" command", hint_style));
    }
    if can_retry && target == ComposeTarget::Prompt {
        right.push(Span::styled("   ", hint_style));
        right.push(Span::styled("/retry", action_key_style));
        if !compact {
            right.push(Span::styled(" retry", hint_style));
        }
    }

    (left, right)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(spans: &[Span<'static>]) -> String {
        spans.iter().map(|span| span.content.to_string()).collect()
    }

    #[test]
    fn prompt_hint_shows_only_enter_send() {
        let theme = Theme::default();
        let (left, right) = hint_row_parts(
            false,
            ActionDensity::Full,
            ComposeTarget::Prompt,
            &theme,
            Color::Reset,
            crate::keymap::Key::TAB,
        );
        assert_eq!(text(&left), "", "idle row carries no nav chords (ADR-0173)");
        assert_eq!(text(&right), "Enter send");
        // ADR-0054: "Enter" span must use theme.keycap_accent() and BOLD
        assert_eq!(right[0].content, "Enter");
        assert_eq!(right[0].style.fg, theme.keycap_accent());
        assert!(right[0].style.add.contains(Modifier::BOLD));
        // " send" span must use keycap_label()
        assert_eq!(right[1].content, " send");
        assert_eq!(right[1].style.fg, theme.keycap_label());
    }

    #[test]
    fn running_hint_shows_toggle_and_send_steer_by_default() {
        let theme = Theme::default();
        let (left, right) = hint_row_parts(
            false,
            ActionDensity::Full,
            ComposeTarget::Running(crate::app::ComposerSendMode::Steer),
            &theme,
            Color::Reset,
            crate::keymap::Key::TAB,
        );
        assert_eq!(text(&left), "Tab follow-up mode");
        assert_eq!(text(&right), "Enter send steer");
    }

    #[test]
    fn running_hint_shows_toggle_and_queue_follow_up_in_follow_up_mode() {
        let theme = Theme::default();
        let (left, right) = hint_row_parts(
            false,
            ActionDensity::Full,
            ComposeTarget::Running(crate::app::ComposerSendMode::FollowUp),
            &theme,
            Color::Reset,
            crate::keymap::Key::TAB,
        );
        assert_eq!(text(&left), "Tab steer mode");
        assert_eq!(text(&right), "Enter queue follow-up");
    }

    #[test]
    fn running_hint_advertises_remapped_toggle_chord() {
        let theme = Theme::default();
        let (left, _) = hint_row_parts(
            false,
            ActionDensity::Full,
            ComposeTarget::Running(crate::app::ComposerSendMode::Steer),
            &theme,
            Color::Reset,
            crate::keymap::Key::CTRL_T,
        );
        assert_eq!(
            text(&left),
            "Ctrl-t follow-up mode",
            "the hint must advertise the effective toggle binding, not the canonical"
        );
    }
}
