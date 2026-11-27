//! Typed slash-command result handling.

use nuo_protocol::RoundEvent;
use nuo_view::model::document::TranscriptMessage;

use super::{E, M, TranslatorState, now_ms};

impl TranslatorState {
    /// Typed slash-command results (acts settle in place; readings are
    /// notices).
    #[allow(clippy::single_match)]
    pub(super) async fn handle_command_event(&mut self, session_id: String, event: RoundEvent) {
        match event {
            RoundEvent::CommandResult {
                name,
                args,
                result,
                effect,
            } => {
                self.mutations.send(M::ClearSwitchingSession).await;
                self.thread_mut(&session_id).retry_progress = None;
                let invocation = if args.is_empty() {
                    format!("/{}", name)
                } else {
                    format!("/{} {}", name, args)
                };
                // The server's declared class decides where the
                // reply is filed, and nothing else does
                // (ADR-0140, `[INV-CMD-08]`). An **act** holds a
                // position in the thread, so it settles the staged
                // row in place (or materialises the fallback if
                // the act happened before this view was watching).
                // A **reading** never touches the thread: its
                // reply is a receipt in the client's notice log,
                // beside every other disclosure, because a
                // measurement of the world is not a thing that was
                // said.
                //
                // Furthermore (ADR-0149, `[INV-CMD-09]`), any failed
                // command or unknown command is withdrawn from the thread:
                // errors never deface L0 transcript, but route to the
                // top-right notice toast and restore the user's draft.
                let is_failure = matches!(
                    result,
                    nuo_protocol::CommandResult::Error { .. }
                        | nuo_protocol::CommandResult::UnknownCommand { .. }
                );
                // The question is "does this occupy a row?", not "is it a reading?".
                // A topology change answers no — its row would belong to a thread that did
                // not exist when the command ran — so it files a receipt and never settles
                // or materialises a transcript row (ADR-0252 §2, INV-VERB-05/06).
                if is_failure || !effect.occupies_a_row() {
                    self.transcript(
                        &session_id,
                        E::WithdrawStagedCommand {
                            invocation: invocation.clone(),
                            restore_draft: is_failure,
                        },
                    )
                    .await;
                    self.mutations
                        .send(M::NoticeDisclosure(
                            crate::shell::app::notice_catalog::ClientNotice::CommandReceipt {
                                invocation,
                                body: result.to_text(),
                                failed: is_failure,
                            }
                            .render(),
                        ))
                        .await;
                } else {
                    let fallback = TranscriptMessage::command_result(
                        name.clone(),
                        args.clone(),
                        Some(result.clone()),
                    )
                    .with_sent_at_ms(now_ms());
                    self.transcript(
                        &session_id,
                        E::SettleCommandResult {
                            invocation,
                            result,
                            fallback: Some(fallback),
                        },
                    )
                    .await;
                }
                if self.thread_mut(&session_id).harness.loop_status.is_idle() {
                    self.chrome(
                        &session_id,
                        crate::shell::event_loop::mutations::ChromeEdit::PhaseOnly(None),
                    )
                    .await;
                }
            }
            _ => {}
        }
    }
}
