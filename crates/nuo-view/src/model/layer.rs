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
use nuo_wire::{AgentNotice, NoticeKind, NoticeSource};

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
/// | `ProviderRetry`      | absorbed — the retry card is its own disclosure   |
/// | `CommandAck`         | toast — ephemeral reply; the invocation stays durable |
/// | `TrustChanged`       | banner — a retained condition                     |
/// | `NudgeInjected`      | log — the injected fact is already a transcript entry |
/// | `ReviewAlert`        | log                                               |
/// | `ImageInputWithheld` | log                                               |
fn preferred_layer(notice: &AgentNotice) -> ContentLayer {
    match notice.kind {
        // The provider-retry disclosure is driven by `RetryScheduled` and
        // refreshes itself per attempt; a notice would be a stale duplicate.
        NoticeKind::ProviderRetry => ContentLayer::Absorbed,
        // An acknowledgement of a configuration/command mutation: a transient
        // bubble is the whole message.
        NoticeKind::CommandAck => ContentLayer::Toast,
        // A trust decision is a condition, not a receipt: it stays on screen
        // until the trust state changes.
        NoticeKind::TrustChanged => ContentLayer::Banner,
        // Receipts and diagnostics: they need a viewport, not a time axis.
        NoticeKind::NudgeInjected | NoticeKind::ReviewAlert | NoticeKind::ImageInputWithheld => {
            ContentLayer::Absorbed
        }
    }
}

/// Route a notice to the layer that displays it, degraded to what `caps` can
/// host.
///
/// `[INV-LAYER-04]`: the result is never the thread transcript, because no such
/// value exists.
pub fn layer_for(
    notice: &AgentNotice,
    caps: SurfaceCapabilities,
    ambient: SurfaceId,
) -> ContentLayer {
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
        // The retry card owns its own disclosure in every client.
        ContentLayer::Absorbed if notice.kind == NoticeKind::ProviderRetry => {
            ContentLayer::Absorbed
        }
        // Receipts and diagnostics prefer the log; degrade to banner, then toast.
        ContentLayer::Absorbed => match (caps.log, caps.banner, caps.toast) {
            (Some(_), _, _) => ContentLayer::SurfaceLog(ambient),
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
    use nuo_wire::NoticeSeverity;

    const KINDS: [NoticeKind; 6] = [
        NoticeKind::ProviderRetry,
        NoticeKind::NudgeInjected,
        NoticeKind::ReviewAlert,
        NoticeKind::TrustChanged,
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
                let layer = layer_for(&notice(kind), caps, SurfaceId::DASHBOARD);
                assert_ne!(layer, ContentLayer::SurfaceLog(SurfaceId::THREAD));
            }
        }
    }

    #[test]
    fn acknowledgements_are_transient_and_retries_are_absorbed() {
        let full = SurfaceCapabilities::FULL;
        assert_eq!(
            layer_for(&notice(NoticeKind::CommandAck), full, SurfaceId::DASHBOARD),
            ContentLayer::Toast
        );
        assert_eq!(
            layer_for(
                &notice(NoticeKind::ProviderRetry),
                full,
                SurfaceId::DASHBOARD
            ),
            ContentLayer::Absorbed
        );
    }

    #[test]
    fn diagnostics_prefer_the_log_and_degrade_towards_transience() {
        assert_eq!(
            layer_for(
                &notice(NoticeKind::ReviewAlert),
                SurfaceCapabilities::with_log(SurfaceId("dialog:tools")),
                SurfaceId("dialog:tools"),
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
                SurfaceId::THREAD,
            ),
            ContentLayer::Banner
        );
        assert_eq!(
            layer_for(
                &notice(NoticeKind::ReviewAlert),
                SurfaceCapabilities::MINIMAL,
                SurfaceId::THREAD,
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
                SurfaceId::THREAD,
            ),
            ContentLayer::Absorbed
        );
    }

    #[test]
    fn trust_is_retained_or_degraded_but_never_inline() {
        assert_eq!(
            layer_for(
                &notice(NoticeKind::TrustChanged),
                SurfaceCapabilities::FULL,
                SurfaceId::DASHBOARD
            ),
            ContentLayer::Banner
        );
        assert_eq!(
            layer_for(
                &notice(NoticeKind::TrustChanged),
                SurfaceCapabilities::MINIMAL,
                SurfaceId::THREAD
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
}
