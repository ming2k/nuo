//! Session hydration, thread metadata and harness snapshot handling.

use std::sync::atomic::Ordering;

use nuo_protocol::{AgentResponse, RoundEvent};

use crate::shell::event_loop;
use crate::shell::transcript::{
    merge_command_rows, merge_round_interrupt_rows, transcript_commands_from_ledger,
    transcript_interrupts_from_records, transcript_messages_from_core,
};

use super::{E, M, TranslatorState};

impl TranslatorState {
    /// Session/thread lifecycle: a switch or hydration replaces the visible
    /// transcript and rebases every thread-scoped client projection.
    pub(super) async fn handle_thread_hydration(&mut self, resp: AgentResponse) {
        match resp {
            AgentResponse::ConversationCleared {
                previous_id,
                thread_id: session_id,
            } => {
                // ADR-0166: the buffer is being replaced, so the session
                // it just left can no longer write into it; the session
                // it is entering is current again.
                self.session_gate.supersede(&previous_id, &session_id);
                // The left session's terminal frames are now dropped by
                // the gate, so its per-round translator state must not
                // leak into the new session's first turn.
                self.retry_progress = None;
                self.reasoning_start = None;
                self.mutations
                    .send(M::Transcript {
                        edit: event_loop::mutations::TranscriptEdit::Clear,
                    })
                    .await;
                self.mutations.send(M::SetRoundCount(0)).await;
                self.needs_round_rebase = false;
                self.mutations.send(M::ClearContextTokens).await;
                // `/new` minted a fresh thread and switched to it: rebase
                // **the tab that represented `previous_id`** — never "the
                // active tab" (ADR-0123 `[INV-TAB-13]`). The two can differ
                // whenever a thread switch raced this frame, and rebasing
                // the wrong one manufactures a tab pointing at a thread
                // that does not exist.
                self.mutations
                    .send(M::LiveSession {
                        previous_id,
                        next_id: session_id.clone(),
                    })
                    .await;
                self.live_session = session_id.clone();
                self.mutations.send(M::TokenReport(None)).await;
                self.mutations
                    .send(M::SessionTree(nuo_protocol::ThreadTree::default()))
                    .await;
            }
            AgentResponse::ConversationReplaced {
                previous_id,
                thread_id: session_id,
                messages,
                commands,
                round_interrupts,
            } => {
                // ADR-0166: see `ConversationCleared` — the replaced
                // buffer must never receive the superseded session's
                // trailing frames, nor inherit its per-round state.
                self.session_gate.supersede(&previous_id, &session_id);
                self.retry_progress = None;
                self.reasoning_start = None;
                self.mutations.send(M::ClearSwitchingSession).await;
                let mut rebuilt = transcript_messages_from_core(messages, &self.tui_config);
                rebuilt = merge_command_rows(rebuilt, transcript_commands_from_ledger(commands));
                rebuilt = merge_round_interrupt_rows(
                    rebuilt,
                    transcript_interrupts_from_records(round_interrupts),
                );
                self.mutations
                    .send(M::Transcript {
                        edit: event_loop::mutations::TranscriptEdit::ReplaceAll {
                            messages: rebuilt,
                        },
                    })
                    .await;
                self.needs_round_rebase = true;
                // The model-window revision changed; do not reuse an API
                // anchor from the previous thread/projection.
                self.mutations.send(M::ClearContextTokens).await;
                // Track the new id so thread-scoped client state follows,
                // and drop the previous thread's cached report. Addressed
                // by `previous_id`, exactly as `ConversationCleared` is
                // (ADR-0123 `[INV-TAB-13]`).
                self.mutations
                    .send(M::LiveSession {
                        previous_id,
                        next_id: session_id.clone(),
                    })
                    .await;
                self.live_session = session_id;
                self.mutations.send(M::TokenReport(None)).await;
                self.mutations
                    .send(M::SessionTree(nuo_protocol::ThreadTree::default()))
                    .await;
            }
            AgentResponse::ThreadSnapshot(snapshot) => {
                let sid = snapshot.thread_id.clone();
                // ADR-0166: a hydration frame names the session the
                // buffer now holds. It cannot be a superseded one, and it
                // repoints the live id that gates thread-scoped replies.
                self.session_gate.hydrate(&sid);
                self.live_session = sid.clone();
                // `[INV-ATTACH-04]` (ADR-0133): this frame is the
                // authoritative hydration payload. Rebuild the whole
                // visible transcript from it (and the command ledger),
                // exactly like `ConversationReplaced`, but addressed to
                // the snapshot's own thread id.
                let mut rebuilt =
                    transcript_messages_from_core(snapshot.messages, &self.tui_config);
                rebuilt =
                    merge_command_rows(rebuilt, transcript_commands_from_ledger(snapshot.commands));
                rebuilt = merge_round_interrupt_rows(
                    rebuilt,
                    transcript_interrupts_from_records(snapshot.round_interrupts),
                );
                self.mutations
                    .send(M::Transcript {
                        edit: event_loop::mutations::TranscriptEdit::ReplaceAll {
                            messages: rebuilt,
                        },
                    })
                    .await;
                self.mutations
                    .send(M::SetRoundCount(snapshot.round_counter))
                    .await;
                self.mutations.send(M::ClearContextTokens).await;
                self.needs_round_rebase = true;
                if let Some(picker) = snapshot.provider_picker {
                    self.mutations.send(M::ProviderPicker(picker)).await;
                }
                if let Some(keys) = snapshot.provider_keys {
                    self.mutations
                        .send(M::KeyStatus(keys.into_iter().collect()))
                        .await;
                }
                if let Some(ct) = snapshot.context_tokens {
                    self.mutations
                        .send(M::ContextTokens {
                            session_id: sid,
                            snapshot: ct,
                        })
                        .await;
                }
                if let Some(harness) = snapshot.harness_state {
                    self.mutations.send(M::Harness(harness)).await;
                }
                if let Some(catalog) = snapshot.command_catalog {
                    self.mutations.send(M::CommandCatalog(catalog)).await;
                }
                self.mutations
                    .send(M::ProviderSwitched {
                        provider: snapshot.provider,
                        model: snapshot.model,
                    })
                    .await;
            }
            _ => {}
        }
    }

    /// Thread-scoped queries and metadata (history, tree/detail views, token
    /// and usage reports, completions, phases).
    pub(super) async fn handle_thread_metadata(&mut self, resp: AgentResponse) {
        match resp {
            AgentResponse::InputHistory(rows) => {
                self.mutations.send(M::InputHistory(rows)).await;
            }
            // Dashboard Archivist search hits. The TUI's
            // Archivist pane lands in a later stage; for now the
            // response is acknowledged (logged) so the wire stays
            // exhaustive without inventing premature UI state.
            AgentResponse::HistorySearch(hits) => {
                tracing::debug!(count = hits.len(), "history search hits");
            }
            AgentResponse::ThreadsOverview(sessions) => {
                self.mutations.send(M::SessionsOverview(sessions)).await;
            }
            AgentResponse::OpenThreadsPanel => {
                self.mutations.send(M::OpenSessionsPanel).await;
            }
            AgentResponse::ThreadTreeSnapshot {
                thread_id: session_id,
                tree,
            } => {
                // A tree query is thread-scoped: reject a reply that
                // raced a primary-thread switch.
                if self.live_session == session_id {
                    self.mutations.send(M::SessionTree(tree)).await;
                }
            }
            AgentResponse::OpenTreePanel => {
                self.mutations.send(M::OpenTreePanel).await;
            }
            AgentResponse::ThreadDetail(detail) => {
                self.mutations.send(M::SessionDetail(detail)).await;
            }
            AgentResponse::ConnectionDetail(detail) => {
                self.mutations.send(M::ConnectionDetail(detail)).await;
            }
            AgentResponse::TokenUsageReport {
                thread_id: session_id,
                report,
            } => {
                // Install the report only when it still belongs to the
                // thread the frontend is viewing — a reply that raced a
                // thread switch would populate the modal with the
                // previous thread's rows.
                let viewed = self.viewed_session_id.lock().await.clone();
                if viewed.as_deref() == Some(session_id.as_str()) {
                    self.mutations.send(M::TokenReport(Some(report))).await;
                }
            }
            AgentResponse::UsageStatsReport { report } => {
                // Thread-independent by design.
                self.mutations.send(M::UsageStats(report)).await;
            }
            AgentResponse::ComposerCompletions {
                request_id,
                text,
                cursor,
                items,
            } => {
                self.mutations
                    .send(M::CompletionSignal(
                        event_loop::mutations::CompletionSignal {
                            request_id,
                            input: text,
                            cursor,
                            items,
                        },
                    ))
                    .await;
            }
            AgentResponse::ThreadContext(snapshot) => {
                self.mutations.send(M::SessionContext(snapshot)).await;
            }
            AgentResponse::ServerEndpointsStatus(report) => {
                self.mutations.send(M::ServerEndpoints(report)).await;
            }
            AgentResponse::ThreadPhase(update) => {
                self.mutations.send(M::SessionPhase(update.phase)).await;
            }
            _ => {}
        }
    }

    /// Harness snapshots and the posture toggles they mirror.
    pub(super) async fn handle_harness_event(&mut self, session_id: String, event: RoundEvent) {
        match event {
            RoundEvent::HarnessState(snapshot) => {
                self.harness.loop_status = snapshot.loop_status;
                self.harness.round_counter = snapshot.round_counter;
                let running = !snapshot.loop_status.is_idle();
                // View-scoped chrome: the authoritative
                // running/idle transition for this session.
                self.chrome(
                    &session_id,
                    event_loop::mutations::ChromeEdit::RoundLifecycle {
                        round_count: snapshot.round_counter,
                        running,
                        can_retry: snapshot.retry_pending && !running,
                    },
                )
                .await;

                // Publish a quarantined snapshot to
                // the PreAttach mount when the per-run gate
                // has not already answered. The applier
                // deduplicates (it mounts only when the
                // interstitial is absent).
                {
                    self.harness.workspace_security = snapshot.workspace_security.clone();
                    let gate_needed = !self.trust_gate_dismissed.load(Ordering::SeqCst)
                        && nuo_view::trust_gate::gate_request(&self.harness.workspace_security)
                            .is_some();
                    if gate_needed {
                        self.mutations
                            .send(M::PreAttach(crate::shell::PreAttachSignal {
                                snapshot: self.harness.workspace_security.clone(),
                            }))
                            .await;
                    }
                }
                if running {
                    // A new round resets the turn counter and
                    // stamps the elapsed-timer origin.
                    self.mutations.send(M::SetCurrentTurn(0)).await;
                    self.mutations
                        .send(M::SetRoundStartedAt(Some(std::time::Instant::now())))
                        .await;
                }
                self.mutations.send(M::SetResponding(running)).await;
                if !running {
                    // The dispatch cycle is complete: any
                    // command component still Pending will
                    // never receive its reply on this pass
                    // mark Cancelled.
                    self.mutations
                        .send(M::Transcript {
                            edit: E::CancelPendingCommands,
                        })
                        .await;
                    self.mutations.send(M::SetPhase(None)).await;
                    self.mutations.send(M::SetCurrentTurn(0)).await;
                    self.mutations.send(M::SetRoundStartedAt(None)).await;
                }
                if !running && self.needs_round_rebase {
                    // A session switch replaced the transcript
                    // before its authoritative idle snapshot
                    // arrived: rebase the reconstructed tail
                    // exactly once, now that the persisted
                    // round counter is known.
                    self.mutations
                        .send(M::Transcript {
                            edit: E::RebaseRounds {
                                round_counter: snapshot.round_counter,
                            },
                        })
                        .await;
                    self.needs_round_rebase = false;
                }
                self.mutations.send(M::Harness(snapshot.clone())).await;

                // A harness state change is always a round
                // boundary. If the previous round ended
                // mid-reasoning, `StreamReasoningEnd` never
                // arrives; freeze the orphaned trace so the
                // spinner stops.
                let duration_ms = self
                    .reasoning_start
                    .take()
                    .map(|started| started.elapsed().as_millis() as u64);
                if !running {
                    self.retry_progress = None;
                    // ADR-0166: a `HarnessState` sample is not a
                    // round boundary. The provisional-row sweep
                    // lives on the round-outcome events
                    // (`RoundCompleted` / `RoundInterrupted` /
                    // `Error`), which *mean* "the round ended".
                    // Making an idle sample harmless requires the
                    // server to publish idle only when it is idle;
                    // that is enforced at every publisher.
                }
                self.transcript(E::FinalizeOrphanedReasoning { duration_ms })
                    .await;
            }
            RoundEvent::TodosUpdated(_) => {}
            RoundEvent::UnattendedChanged(enabled) => {
                self.harness.unattended = enabled;
                self.mutations.send(M::HarnessUnattended(enabled)).await;
            }
            RoundEvent::ConfinementChanged(confined) => {
                self.harness.confined = confined;
                self.mutations.send(M::HarnessConfined(confined)).await;
            }
            _ => {}
        }
    }
}
