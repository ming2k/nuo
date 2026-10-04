//! Disclosure state machine and presentation primitives.
//!
//! A **disclosure** is any collapsible block in the transcript — a tool step, a
//! subagent task, or a reasoning trace — sharing one summary/body model and one
//! color contract. (The leaf renderers keep their kind-specific names —
//! [`draw_tool_step`], [`draw_reasoning_trace`], [`draw_subagent_inline_step`] —
//! since only a tool call really reads as a "step"; this module is the umbrella
//! abstraction over all three.) Historically each kind computed its
//! summary-line color from a tangle of ad-hoc flags (`expanded`, `focused`,
//! `hovered`, status…) scattered across the data, interaction, and render
//! layers. That conflation was the root cause of bugs like "the focused step's
//! text never lights up because the render layer discarded the focus flag".
//!
//! This module models a step's state as **three orthogonal axes**, each with
//! a single reason to change, and reduces the visible presentation to pure
//! functions of them. Renderers feed in the axes; this module owns the
//! mapping to color. The axes are:
//!
//! 1. **Lifecycle** — the underlying operation's run state (Running /
//!    Completed / Failed / Denied / Cancelled). Drives the semantic *accent*
//!    (hue). This axis is **kind-specific** and therefore not unified here:
//!    tool steps carry it via [`crate::tools::ToolStatus`] (5 states),
//!    reasoning traces via a simple running-bool (2 states). The renderer
//!    resolves it to an accent color and passes that in. See
//!    [`summary_text_color`].
//!
//! 2. **Disclosure** — whether the step's body is shown ([`Disclosure`]).
//!    User-controlled, persisted on the message. Shared by every kind.
//!
//! 3. **Interaction** — transient per-frame pointer/keyboard state
//!    ([`Interaction`]). Recomputed from input each draw, never persisted.
//!    Shared by every kind.
//!
//! The presentation contract is three **composable channels**, joined in
//! [`state::summary_text_color`]:
//!
//! - **accent** (hue) — from Lifecycle. A non-completed lifecycle stays
//!   visibly accented even when the step is collapsed and idle, because a
//!   failure/denial must never hide. `Completed` (and reasoning, whose
//!   lifecycle only affects its marker) yield no accent.
//! - **weight** (luminance) — from Disclosure alone, via
//!   [`state::summary_weight`]. Expanded → the primary foreground; collapsed →
//!   muted. Interaction is deliberately **not** a rung on this ladder
//!   (ADR-0174): under the old `muted < hover < fg` model the affordance was
//!   structurally dimmer than the active state, so the hover cue always lost
//!   the salience contest it existed to win.
//! - **affordance** (hue) — from Interaction, via [`state::Interaction::color`]
//!   and the theme's affordance token. Hover/focus tint the summary toward the
//!   affordance hue — "look here, this is interactive" is a channel orthogonal
//!   to "this is open", so a transient cue can never be out-shone by the state
//!   it points at.
//!
//! The channels compose in one place: accent (when present) blends toward the
//! disclosure luminance, then the affordance tint rides on top — so the cue
//! reads identically on plain, accented, and open summaries without ever
//! changing their brightness ordering.

use super::Theme;

pub(crate) mod renderers;
mod state;
pub use renderers::{
    StickyStep, draw_command_result, draw_compacted_card, draw_reasoning_trace,
    draw_sticky_summary_if_needed, draw_subagent_inline_step, draw_tool_step,
};
pub use state::{Disclosure, Interaction, summary_text_color};
