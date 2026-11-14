//! Notice, retry, background job and client operation handling.

use nuo_protocol::{AgentResponse, RoundEvent};
use nuo_view::model::document::{NoticeSeverity, TranscriptMessage};
use nuo_view::phase::Phase;

use crate::shell::event_loop;

use super::{E, M, TranslatorState, now_ms};

impl TranslatorState {
    /// Control-plane notices/errors and local client operations.
    pub(super) async fn handle_notice_and_client_ops(&mut self, resp: AgentResponse) {
        match resp {
            AgentResponse::Error(msg) => {
                self.mutations.send(M::ClearSwitchingSession).await;
                // A control-plane error is a receipt: the toast
                // surface carries it, the conversation does not.
                self.mutations
                    .send(M::NoticeDisclosure(
                        crate::shell::app::notice_catalog::ClientNotice::LocalFault {
                            topic: crate::shell::app::notice_catalog::ClientTopic::Client,
                            message: msg,
                        }
                        .render(),
                    ))
                    .await;
            }
            AgentResponse::TuiLayoutUpdated(_value) => {
                // The apply path already set `app.transcript.transcript_layout`
                // optimistically; a save failure surfaces as `Error`.
            }
            AgentResponse::TuiColorSchemeUpdated { .. } => {
                // Applied optimistically; save failures arrive as `Error`.
            }
            AgentResponse::WebSearchConfigSnapshot(snapshot) => {
                self.mutations
                    .send(M::WebSearchConfig(Some(snapshot)))
                    .await;
            }
            AgentResponse::WebSearchConfigUpdated(snapshot) => {
                // Authoritative post-update ack: re-render from persisted
                // state, discarding any optimistic local edit.
                self.mutations
                    .send(M::WebSearchConfig(Some(snapshot)))
                    .await;
            }
            AgentResponse::CopyToClipboard { text } => {
                let _ = crate::shell::clipboard::copy(&text).await;
            }
            AgentResponse::Exit => {
                self.mutations.send(M::Quit).await;
            }
            _ => {}
        }
    }

    /// Round-level notices and terminal errors.
    pub(super) async fn handle_notice_event(&mut self, session_id: String, event: RoundEvent) {
        match event {
            RoundEvent::Notice(notice) => {
                // While the PreAttach trust gate owns the screen, a
                // trust **failure** is the gate's round-trip outcome
                // (the server could not persist the grant) — deliver it
                // to the surface so the failure renders where the
                // submitting spinner was, instead of burying it in a
                // transcript the full-screen interstitial hides. The
                // applier drops it when no gate is mounted, so
                // in-session failures keep their normal disclosure.
                //
                // Only `TrustFailure` is intercepted. A
                // `TrustChanged` *condition* is what opened the gate in
                // the first place; feeding it here would render the
                // workspace's own state as if it were an error.
                if notice.kind == nuo_tool::NoticeKind::TrustFailure {
                    self.mutations
                        .send(M::PreAttachAttestationError(notice.render_text()))
                        .await;
                }
                // The layer is derived from the surface context,
                // never chosen by the producer and never the
                // thread transcript ([INV-LAYER-04]): a notice is
                // a receipt, not dialogue.
                //
                // `SHELL` declares the notice log, so every
                // disclosure is *retained* there. A fault
                // additionally raises a bubble that names the log;
                // an acknowledgement does not. That inversion is
                // the point: the log is the home, the bubble is a
                // pointer to it.
                let layer =
                    nuo_view::model::layer::layer_for(&notice, nuo_view::model::layer::SHELL);
                if layer != nuo_view::model::layer::ContentLayer::Absorbed {
                    // `RetryScheduled` owns the retry disclosure;
                    // an absorbed receipt is dropped rather than
                    // written into the conversation. The catalog
                    // projects the wire notice into the funnel
                    // payload (ADR-0138), taking the topic from
                    // the closed wire vocabulary.
                    self.mutations
                        .send(M::NoticeDisclosure(
                            crate::shell::app::notice_catalog::RenderedNotice::from_core(&notice),
                        ))
                        .await;
                }
            }
            RoundEvent::Error(e) => {
                let attempts = self
                    .retry_progress
                    .as_ref()
                    .map_or(0, nuo_view::retry::RetryProgress::retries);
                self.retry_progress = None;
                // A terminal round error may still carry the raw
                // retryable-envelope encoding: strip it so the
                // user sees the message, never the wire framing.
                let message = if attempts > 1 {
                    if e.starts_with("Failed after") || e.starts_with("Exhausted") {
                        e
                    } else {
                        format!("Exhausted {attempts} retry attempts — {e}")
                    }
                } else {
                    e
                };
                // A round-level failure is a turn outcome: it
                // holds a position in time (the round ended
                // here) but says nothing to the model.
                //
                // ADR-0166: a terminal error ends the round just
                // as a completion does (`RoundInterrupted` is not
                // emitted for `reason == Error`), so it is the
                // boundary the provisional-row sweep must read.
                // `RoundEvent::Error` is only ever a terminal
                // round error or a pre-round admission refusal —
                // never an error while a round continues.
                self.transcript(E::RetractPendingToolSteps).await;
                let notice = TranscriptMessage::notice(NoticeSeverity::Error, message)
                    .with_sent_at_ms(now_ms());
                self.transcript(E::Append {
                    class: nuo_protocol::EventClass::TurnOutcome,
                    message: notice,
                })
                .await;

                self.mutations.send(M::SetResponding(false)).await;
                self.mutations.send(M::SetPhase(None)).await;

                self.chrome(&session_id, event_loop::mutations::ChromeEdit::RoundEnded)
                    .await;
            }
            _ => {}
        }
    }

    /// Provider retry scheduling.
    #[allow(clippy::single_match)]
    pub(super) async fn handle_retry_event(&mut self, event: RoundEvent) {
        match event {
            RoundEvent::RetryScheduled {
                attempt,
                max_attempts,
                delay_ms,
                message,
            } => {
                // ADR-0166: a scheduled retry is an attempt
                // boundary — the failed attempt is over and its
                // provisional rows cannot become calls. The
                // harness retracted them by id; this sweep makes
                // the invariant hold even if that frame was lost
                // or arrived after the retry.
                self.transcript(E::RetractPendingToolSteps).await;
                let delay = std::time::Duration::from_millis(delay_ms);
                let retry_at = std::time::Instant::now() + delay;
                // Advance the one typed state machine. Every
                // consumer below reads *this* value, so the
                // ordinal on the bar and the ordinal in the
                // receipt are the same number by construction.
                let progress = match self.retry_progress.take() {
                    Some(progress) => {
                        progress.with_next_attempt(attempt, max_attempts, retry_at, message.clone())
                    }
                    None => nuo_view::retry::RetryProgress::scheduled(
                        attempt,
                        max_attempts,
                        retry_at,
                        message.clone(),
                    ),
                };
                let fault = nuo_view::model::notice_text::display_text(&message);
                // The record's ordinal pair, composed by the same
                // owner as the bar's clause (`receipt_label`), so
                // the two can never disagree about which retry
                // this was — and this is the one place the
                // attempt *budget* is stated: `retry 2 of 30`
                // answers "how much rope is left?" for a reader
                // consulting the log afterwards, which is exactly
                // the question the live bar must not spend
                // columns on.
                let receipt = progress.receipt_label();
                // The bar's clause and the stored state are the
                // same value: clone once here so the published
                // setback cannot be a second, independently
                // constructed copy.
                let published = progress.clone();
                self.retry_progress = Some(progress);

                // Publish-only. The clause is retired by the
                // next phase write for *this* session — each
                // store owns its own phase — never by this
                // translator, which is why no arm of this match
                // carries a clause clear. The primary's slot is
                // the App mirror; a background session's is its
                // own chrome entry, so a background retry cannot
                // paint a countdown onto the primary's bar.
                self.mutations.send(M::SetProviderRetry(published)).await;
                // Transport setback, not a workflow phase: the
                // countdown rides the dedicated clause channel.
                self.mutations
                    .send(M::SetPhase(Some(Phase::AwaitingModel)))
                    .await;
                self.mutations.send(M::SetResponding(true)).await;

                // The receipt: the setback is retained in the
                // notice log (a viewport the user can open,
                // scroll and copy from), never in the
                // conversation. One line per scheduled attempt,
                // tagged with the route that failed.
                let (provider, model) = self.attribution();
                let summary = if provider.is_empty() {
                    format!("{receipt} — {fault}")
                } else if model.is_empty() {
                    format!("{receipt} — {provider}: {fault}")
                } else {
                    format!("{receipt} — {provider}/{model}: {fault}")
                };
                self.mutations
                    .send(M::NoticeDisclosure(
                        crate::shell::app::notice_catalog::ClientNotice::ProviderRetryScheduled {
                            summary,
                            // The detail is the fault verbatim so
                            // the provider's full body stays
                            // readable and pasteable in the viewer
                            // now that the transcript no longer
                            // carries it.
                            detail: nuo_view::model::notice_text::display_text(&message),
                        }
                        .render(),
                    ))
                    .await;
            }
            _ => {}
        }
    }

    /// Background job lifecycle notices.
    pub(super) async fn handle_background_job_event(&mut self, event: RoundEvent) {
        match event {
            RoundEvent::BackgroundJobStarted(info) => {
                let label = match &info.spec {
                    nuo_protocol::JobSpec::Process { command, label, .. } => {
                        label.as_deref().unwrap_or(command)
                    }
                    nuo_protocol::JobSpec::Timer { label, prompt, .. } => {
                        label.as_deref().unwrap_or(prompt.as_str())
                    }
                };
                self.mutations
                    .send(M::BackgroundTaskStarted {
                        id: info.id.0.clone(),
                        label: label.to_string(),
                        started_at_ms: info.created_at_ms,
                    })
                    .await;
                let message = TranscriptMessage::notice(
                    NoticeSeverity::Info,
                    format!("Background job started: {label} ({})", info.id.0),
                )
                .with_sent_at_ms(now_ms());
                self.transcript(E::Append {
                    class: nuo_protocol::EventClass::TurnOutcome,
                    message,
                })
                .await;
            }
            RoundEvent::BackgroundJobProgress { .. } => {}
            RoundEvent::BackgroundJobReady { job_id } => {
                // A service task reported readiness.
                let message = TranscriptMessage::notice(
                    NoticeSeverity::Info,
                    format!("Background service ready: {}", job_id.0),
                )
                .with_sent_at_ms(now_ms());
                self.transcript(E::Append {
                    class: nuo_protocol::EventClass::TurnOutcome,
                    message,
                })
                .await;
            }
            RoundEvent::BackgroundJobCompleted(outcome) => {
                let (label, is_success, exit_code, duration_secs) = match &outcome.state {
                    nuo_protocol::JobState::Succeeded { duration_ms, .. } => (
                        format!(
                            "Background job `{}` completed ({}s)",
                            outcome.job_id.0,
                            duration_ms / 1000
                        ),
                        true,
                        Some(0),
                        duration_ms / 1000,
                    ),
                    nuo_protocol::JobState::Failed {
                        duration_ms,
                        exit_code,
                        ..
                    } => (
                        format!(
                            "Background job `{}` failed (exit {exit_code}, {}s)",
                            outcome.job_id.0,
                            duration_ms / 1000
                        ),
                        false,
                        Some(*exit_code),
                        duration_ms / 1000,
                    ),
                    nuo_protocol::JobState::Killed { duration_ms } => (
                        format!(
                            "Background job `{}` terminated ({}s)",
                            outcome.job_id.0,
                            duration_ms / 1000
                        ),
                        false,
                        None,
                        duration_ms / 1000,
                    ),
                    nuo_protocol::JobState::TimedOut { duration_ms } => (
                        format!(
                            "Background job `{}` timed out ({}s)",
                            outcome.job_id.0,
                            duration_ms / 1000
                        ),
                        false,
                        None,
                        duration_ms / 1000,
                    ),
                    _ => (
                        format!("Background job `{}` completed", outcome.job_id.0),
                        true,
                        None,
                        0,
                    ),
                };
                self.mutations
                    .send(M::BackgroundTaskCompleted {
                        id: outcome.job_id.0.clone(),
                        success: is_success,
                        exit_code,
                        duration_secs,
                    })
                    .await;
                let severity = if is_success {
                    NoticeSeverity::Info
                } else {
                    NoticeSeverity::Warning
                };
                let message = TranscriptMessage::notice(severity, label).with_sent_at_ms(now_ms());
                self.transcript(E::Append {
                    class: nuo_protocol::EventClass::TurnOutcome,
                    message,
                })
                .await;
            }
            _ => {}
        }
    }
}
