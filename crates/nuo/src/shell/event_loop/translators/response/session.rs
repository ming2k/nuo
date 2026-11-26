//! Session hydration, thread metadata and harness snapshot handling.

use std::sync::atomic::Ordering;

use protocol::{AgentResponse, RoundEvent};

use crate::shell::event_loop;
use crate::shell::transcript::{
    merge_timeline_rows, transcript_commands_from_ledger, transcript_interrupts_from_records,
    transcript_messages_from_core,
};

use super::{E, M, TranslatorState};

impl TranslatorState {
    /// Session/thread lifecycle: a switch or hydration replaces the visible
    /// transcript and rebases every thread-scoped client projection.
    pub(super) async fn handle_thread_hydration(&mut self, resp: AgentResponse) {
        match resp {
            AgentResponse::ThreadSubscribed { thread_id, phase } => {
                let gen_entry = self
                    .thread_generations
                    .entry(thread_id.clone())
                    .or_insert(0);
                *gen_entry = gen_entry.saturating_add(1).max(1);
                if self.live_session.is_empty() {
                    self.live_session = thread_id.clone();
                }
                let _ = self.thread_mut(&thread_id);
                self.mutations
                    .send(M::ThreadSubscribed { thread_id, phase })
                    .await;
            }
            AgentResponse::ConversationCleared {
                previous_id,
                thread_id: session_id,
            } => {
                // ADR-0253 `[INV-INSTANCE-03]`: retire `previous_id` (generation 0)
                // and advance `session_id`'s generation so trailing frames from the
                // left session are dropped.
                if previous_id != session_id {
                    self.thread_generations.insert(previous_id.clone(), 0);
                }
                let next_gen = self
                    .thread_generations
                    .entry(session_id.clone())
                    .or_insert(0);
                *next_gen = next_gen.saturating_add(1).max(1);
                self.threads.remove(&previous_id);
                {
                    let ts = self.thread_mut(&session_id);
                    ts.retry_progress = None;
                    ts.reasoning_start = None;
                    ts.needs_round_rebase = false;
                }
                self.mutations
                    .send(M::Transcript {
                        thread_id: Some(session_id.clone()),
                        edit: event_loop::mutations::TranscriptEdit::Clear,
                    })
                    .await;
                self.mutations
                    .send(M::SetRoundCount {
                        thread_id: Some(session_id.clone()),
                        round: 0,
                    })
                    .await;
                self.mutations
                    .send(M::ClearContextTokens {
                        thread_id: Some(previous_id.clone()),
                    })
                    .await;
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
                if previous_id != session_id {
                    self.thread_generations.insert(previous_id.clone(), 0);
                }
                let next_gen = self
                    .thread_generations
                    .entry(session_id.clone())
                    .or_insert(0);
                *next_gen = next_gen.saturating_add(1).max(1);
                self.threads.remove(&previous_id);
                {
                    let ts = self.thread_mut(&session_id);
                    ts.retry_progress = None;
                    ts.reasoning_start = None;
                    ts.needs_round_rebase = true;
                }
                self.mutations.send(M::ClearSwitchingSession).await;
                let mut rebuilt = transcript_messages_from_core(messages, &self.tui_config);
                rebuilt = merge_timeline_rows(
                    rebuilt,
                    transcript_commands_from_ledger(commands),
                    transcript_interrupts_from_records(round_interrupts),
                );
                self.mutations
                    .send(M::Transcript {
                        thread_id: Some(session_id.clone()),
                        edit: event_loop::mutations::TranscriptEdit::ReplaceAll {
                            messages: rebuilt,
                        },
                    })
                    .await;
                // The model-window revision changed; do not reuse an API
                // anchor from the previous thread/projection.
                self.mutations
                    .send(M::ClearContextTokens {
                        thread_id: Some(previous_id.clone()),
                    })
                    .await;
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
            }
            AgentResponse::ThreadSnapshot(snapshot) => {
                let sid = snapshot.thread_id.clone();
                // ADR-0166: a hydration frame names the session the
                // buffer now holds. It cannot be a superseded one, and it
                // repoints the live id that gates thread-scoped replies.
                let next_gen = self.thread_generations.entry(sid.clone()).or_insert(0);
                *next_gen = (*next_gen).max(1);
                {
                    let ts = self.thread_mut(&sid);
                    ts.provider = snapshot.provider.clone();
                    ts.model = snapshot.model.clone();
                    ts.needs_round_rebase = true;
                }
                if self.live_session.is_empty() {
                    self.live_session = sid.clone();
                }
                // `[INV-ATTACH-04]` (ADR-0133): this frame is the
                // authoritative hydration payload. Rebuild the whole
                // visible transcript from it (and the command ledger),
                // exactly like `ConversationReplaced`, but addressed to
                // the snapshot's own thread id.
                let mut rebuilt =
                    transcript_messages_from_core(snapshot.messages, &self.tui_config);
                rebuilt = merge_timeline_rows(
                    rebuilt,
                    transcript_commands_from_ledger(snapshot.commands),
                    transcript_interrupts_from_records(snapshot.round_interrupts),
                );
                self.mutations
                    .send(M::Transcript {
                        thread_id: Some(sid.clone()),
                        edit: event_loop::mutations::TranscriptEdit::ReplaceAll {
                            messages: rebuilt,
                        },
                    })
                    .await;
                self.mutations
                    .send(M::SetRoundCount {
                        thread_id: Some(sid.clone()),
                        round: snapshot.round_counter,
                    })
                    .await;
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
                            session_id: sid.clone(),
                            snapshot: ct,
                        })
                        .await;
                }
                if let Some(harness) = snapshot.harness_state {
                    let running = !harness.loop_status.is_idle();
                    self.chrome(
                        &sid,
                        event_loop::mutations::ChromeEdit::RoundLifecycle {
                            round_count: harness.round_counter,
                            running,
                            can_retry: harness.retry_pending && !running,
                        },
                    )
                    .await;
                    self.mutations
                        .send(M::Harness {
                            thread_id: Some(sid.clone()),
                            snapshot: harness,
                        })
                        .await;
                }
                if let Some(catalog) = snapshot.command_catalog {
                    self.mutations.send(M::CommandCatalog(catalog)).await;
                }
                self.mutations
                    .send(M::ProviderSwitched {
                        thread_id: Some(sid),
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
            AgentResponse::ThreadsExistence { missing_thread_ids } => {
                self.mutations
                    .send(M::ReconcileMissingDrafts(missing_thread_ids))
                    .await;
            }
            AgentResponse::ThreadDeleted { thread_id } => {
                self.mutations
                    .send(M::ForgetDeletedThreadDraft(thread_id))
                    .await;
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
            AgentResponse::ContextPlan { thread_id, plan } => {
                // The plan is measured by one `Agent`, so it belongs to exactly
                // one thread: a reply that raced a switch would populate this
                // dialog with another thread's prompt (ADR-0220 §3).
                let viewed = self.viewed_session_id.lock().await.clone();
                if viewed.as_deref() == Some(thread_id.as_str()) {
                    self.mutations.send(M::ContextPlan(plan)).await;
                }
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
                self.mutations
                    .send(M::SessionPhase {
                        thread_id: Some(update.thread_id),
                        phase: update.phase,
                    })
                    .await;
            }
            _ => {}
        }
    }

    /// Harness snapshots and the posture toggles they mirror.
    pub(super) async fn handle_harness_event(&mut self, session_id: String, event: RoundEvent) {
        match event {
            RoundEvent::HarnessState(snapshot) => {
                {
                    let ts = self.thread_mut(&session_id);
                    ts.harness.loop_status = snapshot.loop_status;
                    ts.harness.round_counter = snapshot.round_counter;
                    ts.harness.workspace_security = snapshot.workspace_security.clone();
                }
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
                    let gate_needed = !self.trust_gate_dismissed.load(Ordering::SeqCst)
                        && view::trust_gate::gate_request(&snapshot.workspace_security).is_some();
                    if gate_needed {
                        self.mutations
                            .send(M::PreAttach(crate::shell::PreAttachSignal {
                                snapshot: snapshot.workspace_security.clone(),
                            }))
                            .await;
                    }
                }
                if !running {
                    // The dispatch cycle is complete: any
                    // command component still Pending will
                    // never receive its reply on this pass
                    // mark Cancelled.
                    self.transcript(&session_id, E::CancelPendingCommands).await;
                }
                if !running && self.thread_mut(&session_id).needs_round_rebase {
                    // A session switch replaced the transcript
                    // before its authoritative idle snapshot
                    // arrived: rebase the reconstructed tail
                    // exactly once, now that the persisted
                    // round counter is known.
                    self.transcript(
                        &session_id,
                        E::RebaseRounds {
                            round_counter: snapshot.round_counter,
                        },
                    )
                    .await;
                    self.thread_mut(&session_id).needs_round_rebase = false;
                }
                self.mutations
                    .send(M::Harness {
                        thread_id: Some(session_id.clone()),
                        snapshot: snapshot.clone(),
                    })
                    .await;

                // A harness state change is always a round
                // boundary. If the previous round ended
                // mid-reasoning, `StreamReasoningEnd` never
                // arrives; freeze the orphaned trace so the
                // spinner stops.
                let duration_ms = self
                    .thread_mut(&session_id)
                    .reasoning_start
                    .take()
                    .map(|started| started.elapsed().as_millis() as u64);
                if !running {
                    self.thread_mut(&session_id).retry_progress = None;
                }
                self.transcript(&session_id, E::FinalizeOrphanedReasoning { duration_ms })
                    .await;
            }
            RoundEvent::TodosUpdated(_) => {}
            RoundEvent::UnattendedChanged(enabled) => {
                self.thread_mut(&session_id).harness.unattended = enabled;
                self.mutations
                    .send(M::HarnessUnattended {
                        thread_id: Some(session_id),
                        enabled,
                    })
                    .await;
            }
            RoundEvent::ConfinementChanged(confined) => {
                self.thread_mut(&session_id).harness.confined = confined;
                self.mutations
                    .send(M::HarnessConfined {
                        thread_id: Some(session_id),
                        confined,
                    })
                    .await;
            }
            _ => {}
        }
    }
}
