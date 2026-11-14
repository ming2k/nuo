//! Reasoning stream, system/error notices, and compacted thread cards.
use super::super::helpers::*;
use super::super::types::*;
use super::{CompactedCardData, TranscriptMessage, next_message_id};
use crate::model::interactive::InteractiveEntry;
use crate::model::markdown::{parse_blocks, parse_blocks_verbatim};
use nuo_tool::Role;

impl TranscriptMessage {
    /// Whether this message is a notice with `Error` severity (indicating an
    /// unrecovered turn error, provider failure, or error notice).
    pub fn is_error_notice(&self) -> bool {
        matches!(
            self.kind,
            MessageKind::Notice {
                severity: NoticeSeverity::Error,
                ..
            }
        )
    }

    /// Construct a structured, inspectable compaction checkpoint card.
    pub fn compacted_card(
        archived_messages: usize,
        window_tokens_before: usize,
        window_tokens_after: usize,
        summary: Option<String>,
        tracked_files: Vec<String>,
    ) -> Self {
        let summary_text = summary.clone().unwrap_or_default();
        Self {
            id: next_message_id(),
            role: Role::System,
            blocks: parse_blocks(&summary_text),
            raw: summary_text,
            kind: MessageKind::CompactedCard {
                archived_messages,
                window_tokens_before,
                window_tokens_after,
                summary,
                tracked_files,
                expanded: false,
                user_pinned: false,
            },
            delivery: DeliveryStatus::default(),
            insert_id: None,
            origin_tags: Vec::new(),
            origin: UserMessageOrigin::Chat,
            provider: None,
            model: None,
            effort: None,
            round: None,
            turn: None,
            sent_at_ms: None,
            injection_origin: None,
            rev: 0,
            resume_byte: 0,
            live_blocks: 0,
        }
    }

    pub fn is_compacted_card(&self) -> bool {
        matches!(self.kind, MessageKind::CompactedCard { .. })
    }

    pub fn compacted_card_data(&self) -> Option<CompactedCardData<'_>> {
        match &self.kind {
            MessageKind::CompactedCard {
                archived_messages,
                window_tokens_before,
                window_tokens_after,
                summary,
                tracked_files,
                expanded,
                ..
            } => Some(CompactedCardData {
                archived_messages: *archived_messages,
                tokens_before: *window_tokens_before,
                tokens_after: *window_tokens_after,
                summary: summary.as_deref(),
                tracked_files: tracked_files.as_slice(),
                expanded: *expanded,
            }),
            _ => None,
        }
    }

    /// Whether this row is a round-interrupt marker (C11).
    pub fn is_round_interrupt(&self) -> bool {
        match &self.kind {
            MessageKind::Notice {
                parts: Some(parts), ..
            } => {
                matches!(parts.topic.as_deref(), Some("interrupted") | Some("error"))
            }
            _ => false,
        }
    }

    pub fn reasoning(content: impl Into<String>) -> Self {
        let content = sanitize_text(&content.into()).into_owned();
        let milestones = count_milestones(&content);
        let mut message = Self {
            id: next_message_id(),
            role: Role::Assistant,
            blocks: Vec::new(),
            raw: String::new(),
            kind: MessageKind::Reasoning {
                content: content.clone(),
                duration_ms: None,
                expanded: false,
                user_pinned: false,
                milestones,
            },
            delivery: DeliveryStatus::default(),
            insert_id: None,
            origin_tags: Vec::new(),
            origin: UserMessageOrigin::Chat,
            provider: None,
            model: None,
            effort: None,
            round: None,
            turn: None,
            sent_at_ms: None,
            injection_origin: None,
            rev: 0,
            resume_byte: 0,
            live_blocks: 0,
        };
        message.raw = content;
        message.relayout();
        message
    }

    /// Append a reasoning delta to a still-streaming Reasoning message.
    ///
    /// The accumulated prose (`content`) and the rendered block projection
    /// (`blocks`) advance **together**, exactly as an assistant text stream
    /// does: the append funnels through [`Self::push_stream`], which re-parses
    /// only the live tail and bumps `rev`.
    ///
    /// This is not an optimization detail — it is what makes an *expanded*
    /// live trace render its body at all. The disclosure body is drawn from
    /// `blocks`, so a delta that appended `raw` without re-parsing left every
    /// expansion an empty shell until `StreamReasoningEnd` re-parsed the whole
    /// trace. That defect only ever showed through the subagent folding path
    /// (`push_subagent_event`), which is the one caller of this method; the
    /// top-level stream already used `push_stream` directly.
    pub fn push_reasoning_delta(&mut self, delta: &str) {
        let sanitized = sanitize_text(delta);
        {
            let MessageKind::Reasoning { content, .. } = &mut self.kind else {
                return;
            };
            content.push_str(&sanitized);
        }
        // `push_stream` sanitizes again (idempotent) and owns the incremental
        // re-parse + revision bump, so there is exactly one streaming
        // append mechanism in this type.
        self.push_stream(&sanitized);
    }

    /// Replace a streaming Reasoning message's content with the authoritative
    /// full text and close the trace (stream end / restored session).
    pub fn finalize_reasoning(&mut self, content: &str) {
        let content = sanitize_text(content).into_owned();
        let milestones = count_milestones(&content);
        self.raw = content.clone();
        if let MessageKind::Reasoning {
            content: current,
            milestones: slot,
            ..
        } = &mut self.kind
        {
            *current = content;
            *slot = milestones;
        }
        self.reparse();
    }

    pub fn is_reasoning(&self) -> bool {
        matches!(self.kind, MessageKind::Reasoning { .. })
    }

    /// Whether this message is a harness notice (error / turn-pause / status).
    ///
    /// A live provider retry is **not** a notice: it owns no transcript row at
    /// all (see [`crate::retry`]), so this predicate has exactly one arm.
    pub fn is_notice(&self) -> bool {
        matches!(self.kind, MessageKind::Notice { .. })
    }

    pub fn notice_expanded(&self) -> Option<bool> {
        match &self.kind {
            MessageKind::Notice { expanded, .. } => Some(*expanded),
            _ => None,
        }
    }

    pub fn pin_notice_expanded(&mut self, expanded: bool) {
        self.pin_expanded(expanded);
    }

    /// Construct a round-interrupt marker row (C11) unified as a Notice entry.
    pub fn round_interrupted(record: nuo_protocol::RoundInterrupt) -> Self {
        let severity = match record.reason {
            nuo_protocol::RoundInterruptReason::Error => NoticeSeverity::Error,
            _ => NoticeSeverity::Warning,
        };
        // The reason line names *what happened*; `record.detail` names *why*
        // (the last transport fault, already flattened to one line by the
        // producer). Both belong in the anchor: a stopped round that says only
        // "cancelled" leaves the user without the fault that caused it, and the
        // full payload is in the notice log rather than in this row.
        let fault = record
            .detail
            .as_deref()
            .map(str::trim)
            .filter(|detail| !detail.is_empty());
        let with_fault = |reason: String| match fault {
            Some(detail) => format!("{reason}\n{detail}"),
            None => reason,
        };
        let raw = match record.reason {
            nuo_protocol::RoundInterruptReason::Error => {
                record.detail.clone().unwrap_or_else(|| match record.round {
                    Some(round) => format!("Round {round} — failed with error"),
                    None => "Round failed with error".to_string(),
                })
            }
            _ => match record.round {
                Some(round) => match record.reason {
                    nuo_protocol::RoundInterruptReason::User => {
                        with_fault(format!("Round {round} — cancelled via [Esc Esc]"))
                    }
                    nuo_protocol::RoundInterruptReason::Superseded => {
                        with_fault(format!("Round {round} — superseded by new message"))
                    }
                    nuo_protocol::RoundInterruptReason::Terminated => {
                        with_fault(format!("Round {round} — process exited"))
                    }
                    nuo_protocol::RoundInterruptReason::Error => unreachable!(),
                },
                None => match record.reason {
                    nuo_protocol::RoundInterruptReason::User => {
                        with_fault("Cancelled via [Esc Esc]".to_string())
                    }
                    nuo_protocol::RoundInterruptReason::Superseded => {
                        with_fault("Superseded by new message".to_string())
                    }
                    nuo_protocol::RoundInterruptReason::Terminated => {
                        with_fault("Process exited".to_string())
                    }
                    nuo_protocol::RoundInterruptReason::Error => unreachable!(),
                },
            },
        };
        let parts = match record.reason {
            nuo_protocol::RoundInterruptReason::Error => {
                let parsed = crate::model::notice_text::normalize(&raw);
                NoticeParts {
                    // A provider fault that stopped the round is a *system*
                    // outcome about the round, not a provider receipt: the
                    // provider's own words are the detail, and the attempt
                    // count belongs to the retry state machine.
                    origin: Some(NoticeOrigin::System {
                        topic: SystemNoticeTopic::Interrupted,
                    }),
                    topic: Some("error".to_string()),
                    title: parsed.title,
                    detail: parsed.detail,
                }
            }
            _ => {
                // The reason is the title and the fault is the detail: the two
                // halves are already separated in `raw`, so split rather than
                // duplicating the fault into a single flattened title.
                let parsed = crate::model::notice_text::normalize(&raw);
                NoticeParts {
                    origin: Some(NoticeOrigin::System {
                        topic: SystemNoticeTopic::Interrupted,
                    }),
                    topic: Some("interrupted".to_string()),
                    title: parsed.title,
                    detail: parsed.detail,
                }
            }
        };
        Self::notice(severity, raw).with_notice_parts(parts)
    }

    /// Construct a notice message. Replaces the ad-hoc
    /// `TranscriptMessage::new(Role::System, format!("Error: …"))` pattern with
    /// a typed severity so the renderer can pick color/icon from one place.
    ///
    /// `raw` is transport/harness text; it is normalized **once** here
    /// (`model::notice_text`) into the entry's display text, which is then both
    /// painted verbatim and copied verbatim — one text, three consumers.
    /// Callers with a structured topic/title/detail split chain
    /// [`Self::with_notice_parts`], which re-derives the display text from
    /// those parts.
    pub fn notice(severity: NoticeSeverity, raw: impl Into<String>) -> Self {
        // Notices receive transport and harness errors directly. HTTP proxy
        // bodies commonly use CRLF, and allowing the raw `\r` through to the
        // terminal moves its physical cursor back to column zero while the
        // retained grid still believes it advanced normally. The next diff
        // then paints over unrelated transcript cells. Keep this constructor
        // on the same sanitized boundary as ordinary and retry messages.
        let raw = sanitize_text(&raw.into()).into_owned();
        let text = crate::model::notice_text::display_text(&raw);
        let blocks = parse_blocks_verbatim(&text);
        Self {
            id: next_message_id(),
            role: Role::System,
            blocks,
            raw: text,
            kind: MessageKind::Notice {
                severity,
                parts: None,
                expanded: false,
                user_pinned: false,
            },
            delivery: DeliveryStatus::default(),
            insert_id: None,
            origin_tags: Vec::new(),
            origin: UserMessageOrigin::Chat,
            provider: None,
            model: None,
            effort: None,
            round: None,
            turn: None,
            sent_at_ms: None,
            injection_origin: None,
            rev: 0,
            resume_byte: 0,
            live_blocks: 0,
        }
    }

    /// Construct a notice from its contract form, keeping the
    /// architecture-agreed two-part split intact: the topic label from
    /// [`notice_topic_label`] and the `title`/`body` detail pair. `raw` still
    /// carries the flattened `render_text()` form for copy fidelity and as
    /// the renderer's fallback, but the renderer never has to guess the
    /// split back out of it.
    pub fn notice_from_core(notice: &nuo_tool::AgentNotice) -> Self {
        // Same sanitized boundary as `raw`: provider HTTP bodies commonly
        // carry CRLF, and a raw `\r` reaching the grid moves the physical
        // cursor while the retained grid believes it advanced.
        let origin = match notice.kind {
            nuo_tool::NoticeKind::CheckpointReplay => Some(NoticeOrigin::System {
                topic: SystemNoticeTopic::TurnGuard,
            }),
            nuo_tool::NoticeKind::NudgeInjected => Some(NoticeOrigin::System {
                topic: SystemNoticeTopic::TurnGuard,
            }),
            nuo_tool::NoticeKind::ReviewAlert => Some(NoticeOrigin::System {
                topic: SystemNoticeTopic::Review,
            }),
            nuo_tool::NoticeKind::TrustChanged => Some(NoticeOrigin::System {
                topic: SystemNoticeTopic::Trust,
            }),
            nuo_tool::NoticeKind::TrustFailure => Some(NoticeOrigin::System {
                topic: SystemNoticeTopic::Trust,
            }),
            nuo_tool::NoticeKind::CommandAck => Some(NoticeOrigin::System {
                topic: SystemNoticeTopic::CommandAck,
            }),
            nuo_tool::NoticeKind::ImageInputWithheld => Some(NoticeOrigin::System {
                topic: SystemNoticeTopic::Images,
            }),
            nuo_tool::NoticeKind::Configuration => Some(NoticeOrigin::System {
                topic: SystemNoticeTopic::Configuration,
            }),
        };
        let parts = NoticeParts {
            origin,
            topic: Some(notice_topic_label(notice.kind).to_string()),
            title: sanitize_text(&notice.title).into_owned(),
            detail: notice
                .body
                .as_deref()
                .filter(|body| !body.trim().is_empty())
                .map(|body| sanitize_text(body).into_owned()),
        };
        Self::notice(
            notice_severity_from_core(notice.severity),
            notice.render_text(),
        )
        .with_notice_parts(parts)
    }

    /// Attach the structured topic/detail split to a notice. No-op on
    /// non-notice messages.
    ///
    /// The parts are authoritative: attaching them **re-derives** the entry's
    /// display text (`raw`) and its per-line blocks from `title`/`detail`, so
    /// the entry keeps displaying, selecting, and copying one text. An empty
    /// title carries no structure to project and leaves the text as
    /// constructed.
    pub fn with_notice_parts(mut self, parts: NoticeParts) -> Self {
        if parts.title.trim().is_empty() {
            if let MessageKind::Notice { parts: slot, .. } = &mut self.kind {
                *slot = Some(Box::new(parts));
            }
            return self;
        }
        let text = crate::model::notice_text::NoticeText {
            title: parts.title.clone(),
            detail: parts.detail.clone(),
        }
        .display_text();
        self.raw = text.clone();
        self.blocks = parse_blocks_verbatim(&text);
        self.resume_byte = self.raw.len();
        self.live_blocks = 0;
        if let MessageKind::Notice { parts: slot, .. } = &mut self.kind {
            *slot = Some(Box::new(parts));
        }
        self
    }

    /// The structured topic/detail split, when this notice carries one.
    pub fn notice_parts(&self) -> Option<&NoticeParts> {
        match &self.kind {
            MessageKind::Notice { parts, .. } => parts.as_deref(),
            _ => None,
        }
    }

    /// Project this entry into a paste-ready diagnostic record
    /// (`model::diagnostic`).
    ///
    /// `None` when the entry carries no text — an empty paste is worse than no
    /// paste, so callers surface "nothing to copy" instead.
    pub fn diagnostic(&self) -> Option<crate::model::diagnostic::DiagnosticRecord> {
        crate::model::diagnostic::DiagnosticRecord::from_message(self)
    }

    /// A reasoning trace that has not yet been stamped with a duration — i.e.
    /// its stream is still open. The renderer treats this as the "spinner
    /// should keep breathing" state, and `finalize_streaming_reasoning` uses
    /// it to find orphaned traces to freeze after an interrupt.
    pub fn is_reasoning_streaming(&self) -> bool {
        matches!(
            self.kind,
            MessageKind::Reasoning {
                duration_ms: None,
                ..
            }
        )
    }

    pub fn reasoning_expanded(&self) -> Option<bool> {
        match &self.kind {
            MessageKind::Reasoning { expanded, .. } => Some(*expanded),
            _ => None,
        }
    }

    /// Auto/system disclosure setter — respects a user pin. See
    /// [`Self::set_tool_step_expanded`] for the rationale.
    pub fn set_reasoning_expanded(&mut self, expanded: bool) {
        if let MessageKind::Reasoning {
            expanded: current,
            user_pinned,
            ..
        } = &mut self.kind
        {
            if *user_pinned {
                return;
            }
            *current = expanded;
        }
    }

    /// User-driven disclosure change: force `expanded` and pin it.
    pub fn pin_reasoning_expanded(&mut self, expanded: bool) {
        self.pin_expanded(expanded);
    }

    pub fn set_reasoning_duration(&mut self, duration_ms: u64) {
        if let MessageKind::Reasoning { duration_ms: d, .. } = &mut self.kind {
            *d = Some(duration_ms);
        }
    }

    /// Human-readable summary for the reasoning trace (always one line).
    ///
    /// For models reporting structured thought milestones/outlines (e.g. GPT-5.6
    /// Sol, ChatGPT Responses `reasoning_summary_text`, Claude thinking headings),
    /// this dynamically extracts the active milestone header while streaming
    /// (`Thinking through security architecture components`) and reports the
    /// completed milestone state once finished (`Thought through security
    /// architecture components (1.2s)`).
    ///
    /// Deliberately **no token count**: a local cl100k count of the
    /// visible chain is not the billed reasoning volume — hidden-CoT models
    /// bill far more than they show, and a number that ranges from exact to
    /// 10× off is noise. The authoritative reasoning-token figure lives in the
    /// usage/performance surfaces via `TokenUsage.reasoning_tokens`.
    pub fn reasoning_summary(&self) -> Option<String> {
        let MessageKind::Reasoning {
            content,
            duration_ms,
            milestones,
            ..
        } = &self.kind
        else {
            return None;
        };
        let active_milestone = extract_active_milestone(content);

        Some(match duration_ms {
            None => match active_milestone {
                Some(milestone) => {
                    let topic = normalize_thinking_topic(&milestone);
                    format!("Thinking through {topic}")
                }
                // No milestone to show: an activity word, not a metric.
                None => "Thinking…".to_string(),
            },
            Some(ms) => {
                let duration = duration_text(Some(*ms));
                let milestones = *milestones;
                if milestones > 1 {
                    format!("Thought through {milestones} steps ({duration})")
                } else if milestones == 1
                    && let Some(milestone) = active_milestone
                {
                    let topic = normalize_thinking_topic(&milestone);
                    format!("Thought through {topic} ({duration})")
                } else {
                    format!("Thought ({duration})")
                }
            }
        })
    }
}
