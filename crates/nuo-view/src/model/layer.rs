//! Content-layer routing: the one place that decides *which layer* displays a
//! piece of content (ADR-0116, `[INV-LAYER-02]`, `[INV-LAYER-04]`).
//!
//! # Why the layer is not a field on the content
//!
//! The retired `NoticeSurface` carried the layer on the payload, initialised to
//! "inline" (i.e. the thread transcript). A producer that omitted the field
//! therefore *selected the transcript by accident*, which is how dialogs,
//! configuration replies, connection failures and retrieval receipts ended up in
//! a conversation's scrollback. The layer is a property of the **surface
//! context** — which layers the surface can host, and which one it is — not of
//! the message: the same trust change is a retained banner in a client that has
//! a banner row and a log receipt in a client that does not.
//!
//! # Why the transcript is not a destination
//!
//! [`ContentLayer`] has no transcript variant, so [`layer_for`] cannot select
//! the thread transcript even by mistake — the prohibition is a property of the
//! return type rather than of a test or a comment. Content that genuinely
//! belongs in the transcript (dialogue, turn anchors) is not a notice and is
//! not routed through here at all; see the transcript admission contract in
//! [`crate::model::thread`].
//!
//! # Degradation is a ladder, never a fallback to L0
//!
//! A surface declares what it can host. When the semantically preferred layer is
//! unavailable, the content degrades **towards the most transient hostable
//! layer** (`log → banner → toast`) and is finally absorbed. It never degrades
//! onto the time axis: a client without a log viewer shows a toast rather than
//! scribbling in the conversation.

use crate::model::log::SurfaceId;
use nuo_protocol::{AgentNotice, NoticeKind, NoticeSource};

/// The layers a surface can actually host.
///
/// Declared by the *client*, because "does this surface have a banner row?" is a
/// property of the surface's layout, not of the content or the producer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceCapabilities {
    /// Whether the surface can show transient bubbles.
    pub toast: bool,
    /// Whether the surface has a retained one-row alert area.
    pub banner: bool,
    /// The receipt log this surface can display, if it has a viewer for one.
    pub log: Option<SurfaceId>,
}

impl SurfaceCapabilities {
    /// Toast only: no banner row, no log viewer. The honest description of a
    /// minimal client, and the reason the degradation ladder exists.
    pub const MINIMAL: Self = Self {
        toast: true,
        banner: false,
        log: None,
    };

    /// Toast plus a receipt-log viewer.
    pub const fn with_log(log: SurfaceId) -> Self {
        Self {
            toast: true,
            banner: false,
            log: Some(log),
        }
    }

    /// Every layer this client can host.
    pub const FULL: Self = Self {
        toast: true,
        banner: true,
        log: Some(SurfaceId::DASHBOARD),
    };
}

/// The capabilities of the interactive shell (`nuo::shell`).
///
/// A *declaration about the client*, kept beside the policy that reads it so the
/// two cannot drift. The shell hosts the notice log ([`SurfaceId::NOTICES`]) and
/// has a viewer for it, so the routing table now sends receipts and diagnostics
/// to the log — where they are addressable and scrollable — instead of degrading
/// them onto the time axis.
///
/// No banner row yet: a retained condition (`TrustChanged`) still degrades to a
/// bubble, which is the ladder's next rung and why the log is the *preferred*
/// layer rather than the only one.
pub const SHELL: SurfaceCapabilities = SurfaceCapabilities {
    toast: true,
    banner: false,
    log: Some(SurfaceId::NOTICES),
};

/// The layer a piece of content is displayed in.
///
/// Deliberately exhaustive over *displayable* destinations: there is no
/// transcript arm, because a notice is not dialogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentLayer {
    /// Transient bubble, no history.
    Toast,
    /// Retained condition until it clears or is superseded.
    Banner,
    /// The surface's own receipt log ([`crate::model::log`]).
    SurfaceLog(SurfaceId),
    /// Nothing to display: a dedicated, self-refreshing disclosure already owns
    /// this fact, or the client can host no layer for it.
    Absorbed,
}

impl ContentLayer {
    /// The log surface this layer names, if it is a log layer.
    pub fn log_surface(self) -> Option<SurfaceId> {
        match self {
            Self::SurfaceLog(id) => Some(id),
            _ => None,
        }
    }
}

/// The layer this content *wants*, before the client's capabilities are known.
///
/// The table:
///
/// | kind                 | preferred layer                                  |
/// |----------------------|--------------------------------------------------|
/// | `CheckpointReplay`   | log — a receipt for a tool call that did not run  |
/// | `CommandAck`         | toast — ephemeral reply; the invocation stays durable |
/// | `TrustChanged`       | banner — a retained condition                     |
/// | `NudgeInjected`      | log — the injected fact is already a transcript entry |
/// | `ReviewAlert`        | log                                               |
/// | `ImageInputWithheld` | log                                               |
///
/// There is deliberately no `ProviderRetry` row: a live provider retry is not a
/// notice at all. It is chrome with its own wire event (`RoundEvent::RetryScheduled`)
/// and its own typed state machine ([`crate::retry`]), so it never enters this
/// table and this function has nothing to absorb. The old row said
/// `Absorbed` — "the retry card owns the disclosure" — but the "card" was a
/// transcript row, i.e. the table was documenting a hole rather than a policy.
fn preferred_layer(notice: &AgentNotice) -> ContentLayer {
    match notice.kind {
        // An acknowledgement of a configuration/command mutation: a transient
        // bubble is the whole message.
        NoticeKind::CommandAck => ContentLayer::Toast,
        // A trust decision is a condition, not a receipt: it stays on screen
        // until the trust state changes.
        NoticeKind::TrustChanged => ContentLayer::Banner,
        // Receipts and diagnostics: they need a viewport, not a time axis.
        //
        // `TrustFailure` is a receipt, not a condition: the failed decision is a
        // past event with a cause worth re-reading (and pasting), and there is
        // nothing to keep on screen once the user has seen it. It is a fault, so
        // the log entry still raises a bubble that names the log.
        NoticeKind::TrustFailure
        | NoticeKind::CheckpointReplay
        | NoticeKind::NudgeInjected
        | NoticeKind::ReviewAlert
        | NoticeKind::ImageInputWithheld => ContentLayer::Absorbed,
    }
}

/// Route a notice to the layer that displays it, degraded to what `caps` can
/// host.
///
/// The capability declaration alone decides the destination — there is no
/// second "ambient surface" input, because a client that names a log in its
/// capabilities is exactly the client that can open one. Splitting that fact
/// across two parameters is how content used to end up in a log nobody could
/// read.
///
/// `[INV-LAYER-04]`: the result is never the thread transcript, because no such
/// value exists.
pub fn layer_for(notice: &AgentNotice, caps: SurfaceCapabilities) -> ContentLayer {
    if notice.kind == NoticeKind::TrustChanged {
        return if caps.banner {
            ContentLayer::Banner
        } else if caps.toast {
            // Degraded: a retained condition shown transiently is better than
            // one buried in the conversation.
            ContentLayer::Toast
        } else {
            ContentLayer::Absorbed
        };
    }
    match preferred_layer(notice) {
        // Receipts and diagnostics prefer the log; degrade to banner, then toast.
        ContentLayer::Absorbed => match (caps.log, caps.banner, caps.toast) {
            (Some(log), _, _) => ContentLayer::SurfaceLog(log),
            (None, true, _) => ContentLayer::Banner,
            (None, false, true) => ContentLayer::Toast,
            (None, false, false) => ContentLayer::Absorbed,
        },
        // An acknowledgement is intrinsically transient.
        ContentLayer::Toast => {
            if caps.toast {
                ContentLayer::Toast
            } else if caps.banner {
                ContentLayer::Banner
            } else {
                ContentLayer::Absorbed
            }
        }
        other => other,
    }
}

/// Whether a notice is agent-round context (as opposed to a harness receipt).
///
/// Round-scoped notices are still routed by [`layer_for`], but callers use this
/// to decide whether the notice belongs to the current round's telemetry
/// grouping. It is *not* an admission test for the transcript: nothing here can
/// produce a transcript row.
pub fn is_round_context(notice: &AgentNotice) -> bool {
    matches!(
        notice.source,
        NoticeSource::Agent | NoticeSource::TurnGuard | NoticeSource::Todo
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_protocol::NoticeSeverity;

    const KINDS: [NoticeKind; 7] = [
        NoticeKind::CheckpointReplay,
        NoticeKind::NudgeInjected,
        NoticeKind::ReviewAlert,
        NoticeKind::TrustChanged,
        NoticeKind::TrustFailure,
        NoticeKind::CommandAck,
        NoticeKind::ImageInputWithheld,
    ];

    fn notice(kind: NoticeKind) -> AgentNotice {
        AgentNotice::new(kind, NoticeSeverity::Info, "t", NoticeSource::Harness)
    }

    #[test]
    fn no_kind_in_no_capability_set_reaches_the_thread_transcript() {
        // The return type cannot express the transcript; this asserts that the
        // routing is total and that no capability combination names the thread
        // *log* as a fallback for a notice.
        for caps in [
            SurfaceCapabilities::MINIMAL,
            SurfaceCapabilities::with_log(SurfaceId::DASHBOARD),
            SurfaceCapabilities::FULL,
            SurfaceCapabilities {
                toast: false,
                banner: false,
                log: None,
            },
        ] {
            for kind in KINDS {
                let layer = layer_for(&notice(kind), caps);
                assert_ne!(layer, ContentLayer::SurfaceLog(SurfaceId::THREAD));
            }
        }
    }

    #[test]
    fn acknowledgements_are_transient_and_receipts_are_logged() {
        let full = SurfaceCapabilities::FULL;
        assert_eq!(
            layer_for(&notice(NoticeKind::CommandAck), full),
            ContentLayer::Toast
        );
        // A checkpoint-replay receipt explains a tool call that deliberately
        // did not run: it needs a viewport, not a bubble.
        assert_eq!(
            layer_for(&notice(NoticeKind::CheckpointReplay), full),
            ContentLayer::SurfaceLog(SurfaceId::DASHBOARD)
        );
    }

    #[test]
    fn diagnostics_prefer_the_log_and_degrade_towards_transience() {
        assert_eq!(
            layer_for(
                &notice(NoticeKind::ReviewAlert),
                SurfaceCapabilities::with_log(SurfaceId("dialog:tools")),
            ),
            ContentLayer::SurfaceLog(SurfaceId("dialog:tools"))
        );
        assert_eq!(
            layer_for(
                &notice(NoticeKind::ReviewAlert),
                SurfaceCapabilities {
                    toast: true,
                    banner: true,
                    log: None,
                },
            ),
            ContentLayer::Banner
        );
        assert_eq!(
            layer_for(
                &notice(NoticeKind::ReviewAlert),
                SurfaceCapabilities::MINIMAL,
            ),
            ContentLayer::Toast,
            "a client with no log viewer shows a bubble rather than writing to the conversation"
        );
        assert_eq!(
            layer_for(
                &notice(NoticeKind::ReviewAlert),
                SurfaceCapabilities {
                    toast: false,
                    banner: false,
                    log: None,
                },
            ),
            ContentLayer::Absorbed
        );
    }

    #[test]
    fn trust_is_retained_or_degraded_but_never_inline() {
        assert_eq!(
            layer_for(&notice(NoticeKind::TrustChanged), SurfaceCapabilities::FULL),
            ContentLayer::Banner
        );
        assert_eq!(
            layer_for(
                &notice(NoticeKind::TrustChanged),
                SurfaceCapabilities::MINIMAL
            ),
            ContentLayer::Toast
        );
    }

    #[test]
    fn command_ack_is_not_round_context_but_a_nudge_is() {
        assert!(!is_round_context(&notice(NoticeKind::CommandAck)));
        let nudge = AgentNotice::new(
            NoticeKind::NudgeInjected,
            NoticeSeverity::Info,
            "t",
            NoticeSource::TurnGuard,
        );
        assert!(is_round_context(&nudge));
    }

    #[test]
    fn the_shell_declaration_routes_diagnostics_into_its_notice_log() {
        // The defect this locks down: with `MINIMAL`, every diagnostic degraded
        // to a bubble — one slot, two seconds, then gone. The shell declares a
        // viewer, so the routing table must send receipts to that log instead.
        for kind in [
            NoticeKind::ReviewAlert,
            NoticeKind::ImageInputWithheld,
            NoticeKind::NudgeInjected,
        ] {
            assert_eq!(
                layer_for(&notice(kind), SHELL),
                ContentLayer::SurfaceLog(SurfaceId::NOTICES),
                "{kind:?} must be retained, not bubbled"
            );
        }
        // A `CommandAck` is an acknowledgement of something the user just did,
        // so it stays transient even though the log exists.
        assert_eq!(
            layer_for(&notice(NoticeKind::CommandAck), SHELL),
            ContentLayer::Toast
        );
    }

    #[test]
    fn a_declared_log_wins_over_the_ambient_surface() {
        // Content must never be written into a log the client cannot open. The
        // capability declaration is the authority; there is no second input.
        assert_eq!(
            layer_for(&notice(NoticeKind::ReviewAlert), SHELL),
            ContentLayer::SurfaceLog(SurfaceId::NOTICES),
            "not the ambient thread surface, which owns the transcript instead"
        );
    }

    #[test]
    fn a_trust_failure_is_a_receipt_while_a_trust_change_is_a_condition() {
        // The split exists so the client can act on the two differently: a
        // condition waits for a human decision and must stay on screen, while a
        // failure is a past event with a cause worth re-reading. Sharing one
        // kind forced the mounted gate to intercept every `TrustChanged` and
        // hope none of them was merely an escalation.
        assert_eq!(
            layer_for(&notice(NoticeKind::TrustChanged), SurfaceCapabilities::FULL),
            ContentLayer::Banner,
        );
        assert_eq!(
            layer_for(&notice(NoticeKind::TrustFailure), SHELL),
            ContentLayer::SurfaceLog(SurfaceId::NOTICES),
            "a failed decision belongs in the retained log, with its cause"
        );
        // It is a fault, so it also bubbles and names the log. A condition does
        // not: the gate or the banner owns it.
        assert!(AgentNotice::trust_failure("x").severity == NoticeSeverity::Error);
        assert!(AgentNotice::trust_changed("x").severity == NoticeSeverity::Warning);
    }
}
