//! Canonical Action Control Status and Permissibility (ADR-0110).
//!
//! Provides the universal lifecycle and rendering contract for interactive
//! controls, buttons, and action triggers across Nuo UI.
//!
//! Enforces:
//! - `[INV-TUI-RUNNING-01]` (Strict Lifecycle-Tone Separation): `theme.dim()` is
//!   strictly reserved for dead/inert/disabled actions. Running states MUST use
//!   active accent hues (`theme.info()` / `theme.warning`).
//! - `[INV-TUI-RUNNING-02]` (Action Permissibility Tri-Partition): Running controls
//!   guard against re-entrant triggers while preserving inspection and abort paths.

use nuotc::{Color, Modifier, Style};

use crate::theme::Theme;

/// Universal lifecycle state of an interactive control or action trigger (ADR-0110).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ActionStatus {
    /// Actionable and ready for user interaction.
    #[default]
    Idle,
    /// Asynchronous work in flight. Mutating re-triggers are guarded (no-op),
    /// while visual feedback is bright and active.
    Running {
        spinner_frame: usize,
        elapsed_secs: Option<u64>,
    },
    /// Permanently or provisionally locked/inert. Clicks and triggers are no-ops.
    Disabled { reason: Option<String> },
}

impl ActionStatus {
    /// Construct a running action state with optional elapsed duration.
    pub fn running(spinner_frame: usize, elapsed_secs: Option<u64>) -> Self {
        Self::Running {
            spinner_frame,
            elapsed_secs,
        }
    }

    /// Construct a disabled action state with explanation.
    pub fn disabled(reason: impl Into<String>) -> Self {
        Self::Disabled {
            reason: Some(reason.into()),
        }
    }

    /// Whether this action is in an active running/busy state.
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }

    /// Whether this action is disabled/locked.
    pub fn is_disabled(&self) -> bool {
        matches!(self, Self::Disabled { .. })
    }

    /// Whether this action is eligible to be triggered (only [`Self::Idle`]).
    ///
    /// Per `[INV-TUI-RUNNING-02]`, running and disabled actions MUST guard
    /// against re-entrant execution.
    pub fn is_actionable(&self) -> bool {
        matches!(self, Self::Idle)
    }

    /// Format the button or label text with appropriate liveness indicators.
    pub fn format_label(&self, base_label: &str, running_label: Option<&str>) -> String {
        match self {
            Self::Idle => format!(" {base_label} "),
            Self::Running {
                spinner_frame,
                elapsed_secs,
            } => {
                const SPINNER_GLYPHS: [&str; 10] =
                    ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                let spinner = SPINNER_GLYPHS[*spinner_frame % SPINNER_GLYPHS.len()];
                let label = running_label.unwrap_or(base_label);
                match elapsed_secs {
                    Some(s) => format!(" {spinner} {label} ({s}s) "),
                    None => format!(" {spinner} {label} "),
                }
            }
            Self::Disabled { .. } => format!(" {base_label} "),
        }
    }

    /// Compute the presentation style for the action control.
    ///
    /// Per `[INV-TUI-RUNNING-01]`:
    /// - `Idle`: foreground text, brand/accent highlight when focused.
    /// - `Running`: active accent hue (`theme.info()`), BOLD, NEVER `theme.dim()`.
    /// - `Disabled`: muted `theme.dim()`, no bold modifier.
    pub fn style(&self, theme: &Theme, is_focused: bool, default_border: Color) -> Style {
        match self {
            Self::Idle => {
                if is_focused {
                    Style::default()
                        .fg(theme.app_bg)
                        .bg(default_border)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.text_muted).bg(Color::Reset)
                }
            }
            Self::Running { .. } => {
                // Invariant [INV-TUI-RUNNING-01]: Active accent hue, never dimmed
                if is_focused {
                    Style::default()
                        .fg(theme.app_bg)
                        .bg(theme.info())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(theme.info())
                        .bg(Color::Reset)
                        .add_modifier(Modifier::BOLD)
                }
            }
            Self::Disabled { .. } => {
                // Invariant [INV-TUI-RUNNING-01] & ADR-0076: Dim tone, stripped bold
                Style::default().fg(theme.dim()).bg(Color::Reset)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inv_tui_running_01_lifecycle_tone_demarcation() {
        let theme = Theme::default();
        let border = theme.system_fg;

        // 1. Running state must use active info hue, bold, and never dim tone
        let running = ActionStatus::running(0, Some(2));
        let running_style = running.style(&theme, false, border);
        assert_eq!(running_style.fg, theme.info());
        assert!(running_style.add.contains(Modifier::BOLD));
        assert_ne!(
            running_style.fg,
            theme.dim(),
            "[INV-TUI-RUNNING-01] Running state must never use theme.dim()"
        );

        // 2. Disabled state must use theme.dim() and stripped bold
        let disabled = ActionStatus::disabled("requires subscription");
        let disabled_style = disabled.style(&theme, false, border);
        assert_eq!(
            disabled_style.fg,
            theme.dim(),
            "[INV-TUI-RUNNING-01] Disabled state must use theme.dim()"
        );
        assert!(
            !disabled_style.add.contains(Modifier::BOLD),
            "[INV-TUI-RUNNING-01] Disabled state must strip bold"
        );
    }

    #[test]
    fn test_inv_tui_running_02_action_permissibility() {
        let idle = ActionStatus::Idle;
        let running = ActionStatus::running(1, None);
        let disabled = ActionStatus::disabled("locked");

        assert!(idle.is_actionable());
        assert!(
            !running.is_actionable(),
            "[INV-TUI-RUNNING-02] Running action must guard re-entry"
        );
        assert!(
            !disabled.is_actionable(),
            "[INV-TUI-RUNNING-02] Disabled action must be inert"
        );
    }

    #[test]
    fn test_action_status_format_label() {
        let idle = ActionStatus::Idle;
        assert_eq!(idle.format_label("Apply", None), " Apply ");

        let running_no_elapsed = ActionStatus::running(0, None);
        assert_eq!(
            running_no_elapsed.format_label("Apply", Some("Applying")),
            " ⠋ Applying "
        );

        let running_with_elapsed = ActionStatus::running(2, Some(4));
        assert_eq!(
            running_with_elapsed.format_label("Apply", Some("Applying")),
            " ⠹ Applying (4s) "
        );

        let disabled = ActionStatus::disabled("Permission denied");
        assert_eq!(disabled.format_label("Apply", None), " Apply ");
    }
}
