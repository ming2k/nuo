//! Input history: record/clear, attachment pruning, session backfill, prev/next recall, draft restore.

use super::*;

impl App {
    pub(crate) const HISTORY_ATTACHMENTS_CAP: usize = 32;

    /// The composer's inline-recall **pointer badge**, derived for the
    /// composer's top chrome row (`[history 3/17 · draft saved]`).
    ///
    /// `None` while [`Self::history_index`] is `None` — the badge must not
    /// exist in draft mode: ADR-0173/0174's zero-mode-indication-tax stance
    /// means the only state worth announcing is the one the user can enter
    /// silently and lose work from, and the ↑/↓ recall pointer is exactly
    /// that state (the composer swaps its content under the user's feet).
    ///
    /// `position` is **1-based** (`history_index + 1`) because the badge is
    /// prose for humans, not an index into the slice; `total` is the current
    /// session's row count, computed fresh from [`Self::current_session_history`]
    /// (cheap: the slice rebuilds on every render anyway via the arrow paths,
    /// and a stale cached total would disagree with the live walk).
    ///
    /// `edited` is true when the composer's live text no longer matches the
    /// loaded row: the pointer still addresses row `p`, but the buffer is a
    /// user-modified fork of it. The badge says `· edited` so the indicator
    /// never lies about what the buffer holds — the pointer model (see
    /// `App::history_index`) treats history-row edits as temporary, but a
    /// user who edits the recalled text deserves to see that fact before
    /// pressing ↑ and silently discarding it.
    ///
    /// The `draft saved` reassurance is *not* computed here — it is a
    /// presentation fact about [`Self::history_draft`] (non-empty vs empty),
    /// appended by the renderer. This function answers only "where is the
    /// pointer and has the buffer forked".
    pub fn history_recall_badge(&self) -> Option<(usize, usize, bool)> {
        let position = self.history_index?;
        let rows = self.current_session_history();
        if rows.is_empty() {
            // A non-None pointer over an empty slice cannot survive a
            // navigation step, but a session switch could theoretically land
            // here between the pointer reset and the next render — never
            // render a `0/0` badge that addresses nothing.
            return None;
        }
        // The pointer is a position in the newest-first slice; the entry
        // lookup needs the combined-space row index the slice stores.
        let edited = self
            .history_entry(rows[position.min(rows.len() - 1)])
            .is_some_and(|entry| entry.text != self.input);
        Some((position + 1, rows.len(), edited))
    }

    /// Rows shown in the Ctrl+R history panel, as `(original_index,
    /// FuzzyMatch)` pairs indexing into [`App::input_history`]. The single
    /// source of truth for navigation (Up/Down clamp), Enter-accept, and
    /// rendering — they all index into this same vector so the cursor never
    /// lands on a row the user cannot see.
    ///
    /// The list is always the **whole cross-session history**, independent of
    /// which session or workspace produced each entry — that is the entire
    /// point of Ctrl+R (the inline ↑/↓ recall, by contrast, is scoped to the
    /// current session via [`App::current_session_history`]). Entries are
    /// ordered newest-first by `created_at_ms`.
    ///
    /// With an empty query (`App::input`, which the panel borrows as its live
    /// filter) every entry shows, unhighlighted. Once a query is present the
    /// rows are the fuzzy-ranked matches, best score first, with the original
    /// newest-first order as the stable tiebreaker. Recomputed from scratch
    /// each call: history is small and this runs at most a few times per
    /// frame, so caching would only add stale-state risk.
    pub fn history_rows(&self) -> Vec<(usize, fuzzy::FuzzyMatch)> {
        // The display order: newest-first. The on-disk file is already stored
        // newest-first, but in-memory appends during this run land at the
        // tail, so re-sort by created_at_ms (stable) to keep the panel's order
        // correct without mutating the stored Vec.
        let order: Vec<usize> = self.history_order();
        let query = self.surfaces.dlg::<crate::surfaces::HistorySearchDialog>().query.text.as_str();
        if query.is_empty() {
            // Empty query → show everything newest-first, unhighlighted.
            return order
                .into_iter()
                .map(|i| {
                    (
                        i,
                        fuzzy::FuzzyMatch {
                            score: 0,
                            positions: Vec::new(),
                        },
                    )
                })
                .collect();
        }
        let max_ts = order
            .first()
            .and_then(|&i| self.input_history.get(i))
            .map(|e| e.created_at_ms)
            .unwrap_or(0);

        // Zero-allocation streaming rank: matches are filtered and scored in a single
        // pass directly over the history entries without intermediate string allocations.
        // `rank_iter` preserves the input `order` on ties (stable).
        let items = order
            .iter()
            .filter_map(|&i| self.input_history.get(i).map(|e| (i, e.text.as_str())));
        let mut ranked = fuzzy::rank_iter(items, query);

        // Blend in recency decay and current session affinity (Industry Gold Standard)
        for (orig_idx, m) in &mut ranked {
            if let Some(entry) = self.input_history.get(*orig_idx) {
                // 1. Recency bonus: smooth decay based on age relative to newest item
                if max_ts > 0 && entry.created_at_ms > 0 {
                    let age_ms = max_ts.saturating_sub(entry.created_at_ms);
                    const HOUR_MS: u64 = 3_600_000;
                    const DAY_MS: u64 = 86_400_000;
                    const WEEK_MS: u64 = 7 * DAY_MS;
                    const MONTH_MS: u64 = 30 * DAY_MS;

                    let recency_bonus = if age_ms < HOUR_MS {
                        40
                    } else if age_ms < DAY_MS {
                        25
                    } else if age_ms < WEEK_MS {
                        15
                    } else if age_ms < MONTH_MS {
                        5
                    } else {
                        0
                    };
                    m.score = m.score.saturating_add(recency_bonus);
                }

                // 2. In-session affinity: items sent in this exact thread have high contextual relevance
                if !self.current_session_id.is_empty()
                    && entry.session_id.as_deref() == Some(self.current_session_id.as_str())
                {
                    m.score = m.score.saturating_add(20);
                }
            }
        }

        fuzzy::sort_by_score(&mut ranked);
        ranked
    }

    /// The newest-first ordering of [`App::input_history`] by `created_at_ms`,
    /// as original indices into that Vec. Stable on ties so the on-disk order
    /// survives. Shared by [`Self::history_rows`] (Ctrl+R) and
    /// [`Self::current_session_history`] (inline ↑/↓) so both surfaces agree
    /// on what "newest" means.
    pub fn history_order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.input_history.len()).collect();
        // Newest-first by `created_at_ms`; entries stamped within the same
        // millisecond (a fast send burst) break ties by insertion order — the
        // later index is the newer prompt — so "newest-first" stays
        // well-defined instead of degrading to oldest-first on a tie.
        order.sort_by(|&a, &b| {
            self.input_history[b]
                .created_at_ms
                .cmp(&self.input_history[a].created_at_ms)
                .then_with(|| b.cmp(&a))
        });
        order
    }

    /// The current session's history, newest-first. This is what the inline
    /// ↑/↓ recall walks: the union of the **persisted** history
    /// ([`Self::input_history`], filtered to entries whose `session_id`
    /// matches [`App::current_session_id`]) and the **derived** transcript
    /// rows ([`Self::session_history_backfill`]), so arrow-key recall
    /// surfaces exactly the prompts of *this* thread — including ones
    /// this client never recorded (a session resumed from elsewhere). Ctrl+R
    /// is unaffected — it searches the whole persisted list regardless of
    /// session.
    ///
    /// Returns indices into the combined row space: `0..input_history.len()`
    /// address the persisted store, `input_history.len() + i` addresses the
    /// `i`-th backfill row. [`Self::history_entry`] resolves either kind, so
    /// callers never branch on the boundary.
    pub fn current_session_history(&self) -> Vec<usize> {
        let sid = self.current_session_id.as_str();
        let mut rows: Vec<(u64, usize)> = self
            .input_history
            .iter()
            .enumerate()
            .filter(|(_, e)| e.session_id.as_deref() == Some(sid))
            .map(|(i, e)| (e.created_at_ms, i))
            .collect();
        let base = self.input_history.len();
        rows.extend(
            self.session_history_backfill
                .iter()
                .enumerate()
                // Walked newest-first below, so the backfill's oldest-first
                // storage order must be reversed to reach `created_at_ms`
                // parity — ties against persisted rows resolve to the
                // transcript's own (older-first) order via the stable sort.
                .map(|(i, e)| (e.created_at_ms, base + i)),
        );
        // Newest-first: stable sort keeps within-store order on ties, and the
        // backfill rows (transcript append order) follow persisted rows of the
        // same millisecond.
        rows.sort_by_key(|&(created_at_ms, _)| std::cmp::Reverse(created_at_ms));
        rows.into_iter().map(|(_, i)| i).collect()
    }

    /// Resolve a row index from [`Self::current_session_history`] to its
    /// entry, transparently spanning the persisted store (`0..len`) and the
    /// session backfill (`len..`). `None` when the index is out of range.
    pub fn history_entry(&self, idx: usize) -> Option<&nuo_wire::HistoryEntry> {
        if idx < self.input_history.len() {
            self.input_history.get(idx)
        } else {
            self.session_history_backfill
                .get(idx - self.input_history.len())
        }
    }

    /// Drop backfill rows whose text this session has since **recorded**
    /// (the send path persisted it, possibly by re-tagging an existing
    /// global-dedup row into this session). Called after
    /// [`Self::record_input_history`] so the union the ↑/↓ walk sees never
    /// contains the same prompt twice: without this, a prompt that was
    /// backfilled on resume and then re-sent through this client would
    /// surface as two adjacent rows.
    pub fn prune_backfill_after_record(&mut self, text: &str) {
        self.session_history_backfill.retain(|e| e.text != text);
    }

    /// Seed [`Self::session_history_backfill`] with the **viewed
    /// transcript's** genuine chat prompts, so the inline ↑/↓ recall reflects
    /// the thread the user is actually looking at rather than only what
    /// this client's database happens to contain.
    ///
    /// This is the resume path: `ConversationReplaced` hands the TUI another
    /// session's transcript, and prompts typed into that session by a
    /// *different* client (or before this session existed) were never
    /// recorded locally. Without the backfill, `↑` after a resume comes up
    /// empty even though the thread visibly contains prompts. The
    /// initial startup transcript is backfilled the same way before the
    /// first frame.
    ///
    /// Only `UserMessageOrigin::Chat` rows count — slash commands
    /// (`/model`, …) and `!shell` passthroughs are UI gestures excluded from
    /// the history by contract (`[input_history] record_commands = false`),
    /// and queued-but-unsent rows are not prompts yet. A prompt already
    /// recorded by this client (present in the persisted history under this
    /// session) is skipped, so live sends and backfills never duplicate a
    /// row.
    ///
    /// The backfill is **derived state, never persisted**: transcript rows
    /// already live in the session store (the durable source of truth),
    /// so writing them into input history would duplicate the
    /// store and race the cross-process merge. Timestamps come from the
    /// transcript where available (`sent_at_ms`, falling back to `now_ms`
    /// for legacy rows so ordering stays stable).
    ///
    /// `tail` is the transcript's unconsumed suffix as `(text, is_chat,
    /// sent_at_ms)` triples — copied out by the caller, which cannot lend
    /// the transcript while `App` is borrowed mutably here.
    pub fn backfill_session_history(&mut self, tail: &[(String, bool, u64)], now_ms: u64) {
        let sid = self.current_session_id.as_str();
        let recorded: HashSet<&str> = self
            .input_history
            .iter()
            .filter(|e| e.session_id.as_deref() == Some(sid))
            .map(|e| e.text.as_str())
            .collect();
        for (text, is_chat, sent_at_ms) in tail {
            if !is_chat || text.is_empty() || recorded.contains(text.as_str()) {
                continue;
            }
            // Same prompt twice in one thread (an intentional resend)
            // is one recallable row — the newest position wins, matching the
            // persisted history's newest-first contract.
            if let Some(existing) = self
                .session_history_backfill
                .iter_mut()
                .find(|e| e.text == *text)
            {
                existing.created_at_ms = (*sent_at_ms).max(existing.created_at_ms);
                continue;
            }
            self.session_history_backfill
                .push(nuo_wire::HistoryEntry::new(
                    text.clone(),
                    Some(self.current_session_id.clone()),
                    Some(self.current_workspace.clone()),
                    if *sent_at_ms == 0 {
                        now_ms
                    } else {
                        *sent_at_ms
                    },
                ));
        }
    }

    /// Record `entry` in the cross-session input history, tagged with the
    /// current session id + workspace and stamped "now": reset the up/down
    /// recall cursor, dedup against the most recent same-text+same-session
    /// entry, and persist the new entry to disk immediately (off-thread) so
    /// it survives an unclean exit and is visible to concurrent sessions
    /// right away rather than only on exit.
    ///
    /// `images` / `text_pastes` are the attachments staged behind the chips
    /// in `entry` at send time. They are **not** persisted into SQLite (input history is
    /// rebuildable cosmetic telemetry, never thread data)
    /// but are cached in memory keyed by the entry's `(text, session_id)`
    /// identity, so the ↑/↓ and Ctrl+R recall paths can restore a just-sent
    /// or interrupted message's attachments instead of shipping a bare chip
    /// label the model would read as literal text.
    ///
    /// The origin (session/workspace) is what separates Ctrl+R (searches the
    /// whole history) from inline ↑/↓ (walks only this session's entries).
    pub fn record_input_history(
        &mut self,
        entry: String,
        images: Vec<ImagePart>,
        text_pastes: Vec<String>,
    ) {
        self.history_index = None;
        if entry.is_empty() && images.is_empty() && text_pastes.is_empty() {
            return;
        }
        // Slash-command invocations (`/model`, `/new`, …) are UI gestures,
        // not prompts: they are already visible in the transcript, and most
        // users don't want `/model` noise cluttering the Ctrl+R picker. Skip
        // them unless `[input_history] record_commands` opts them back in.
        if entry.starts_with('/') && !self.input_history_record_commands {
            return;
        }
        let now = crate::event_loop::now_epoch_ms();
        // Ensure strictly-increasing timestamps. `now_epoch_ms()` can return
        // the same millisecond for a rapid burst of sends, and the history
        // order's stable sort would then keep input order — putting the
        // older prompt ahead of the newer one and breaking the newest-first
        // contract (the inline ↑ would land on the stale entry first). The
        // wall clock stays the baseline; when it has not advanced past the
        // newest recorded entry, nudge the stamp forward by one.
        let latest_ts = self
            .input_history
            .iter()
            .map(|e| e.created_at_ms)
            .max()
            .unwrap_or(0);
        let now = if now > latest_ts {
            now
        } else {
            latest_ts.saturating_add(1)
        };
        let session_id = if self.current_session_id.is_empty() {
            None
        } else {
            Some(self.current_session_id.clone())
        };
        let workspace = if self.current_workspace.is_empty() {
            None
        } else {
            Some(self.current_workspace.clone())
        };
        // Cache the attachments first (before the dedup early-return) so a
        // repeat send of the same prompt refreshes the payloads a recall
        // will restore, even though no new history row is pushed.
        if !images.is_empty() || !text_pastes.is_empty() {
            let identity = (entry.clone(), session_id.clone());
            if !self.history_attachments.contains_key(&identity) {
                self.history_attachments_order.push_back(identity.clone());
            }
            self.history_attachments.insert(
                identity,
                HistoryAttachments {
                    images,
                    text_pastes,
                },
            );
            self.prune_history_attachments();
        }
        // With `[input_history] dedup` (default on) the prompt text alone is
        // the identity: the same prompt sent twice — even in a different
        // session — stays one row. Re-sending refreshes the timestamp (so the
        // entry bubbles to the top of the newest-first picker) and adopts the
        // newest known origin (so ↑/↓ in the session that last sent it still
        // finds it), then persists the refreshed entry.
        if self.input_history_dedup {
            if let Some(existing) = self.input_history.iter_mut().find(|e| e.text == entry) {
                existing.created_at_ms = now;
                if session_id.is_some() {
                    existing.session_id = session_id;
                }
                if workspace.is_some() {
                    existing.workspace = workspace;
                }
                let refreshed = existing.clone();
                // The text is now recorded under this session: drop any
                // transcript-derived backfill row for it so the ↑/↓ union
                // never shows the same prompt twice.
                self.prune_backfill_after_record(&refreshed.text);
                if self.input_history_persist {
                    // Server-side SSOT merge (ADR-0197): the TUI never opens
                    // the shared SQLite store itself.
                    self.send_intent(nuo_wire::AgentRequest::RecordInputHistory {
                        entries: vec![refreshed],
                        dedup: true,
                    });
                }
                return;
            }
            let recorded = nuo_wire::HistoryEntry::new(entry, session_id, workspace, now);
            self.push_history(recorded.clone());
            if self.input_history_persist {
                self.send_intent(nuo_wire::AgentRequest::RecordInputHistory {
                    entries: vec![recorded],
                    dedup: true,
                });
            }
            return;
        }
        // Dedup disabled: dedup against the newest same-text entry in *this*
        // session — typing the same prompt twice in a row should not produce
        // two adjacent rows, but the same words typed in a different session
        // legitimately are a distinct history entry (each keeps its own
        // origin).
        let already_latest_in_session = self
            .current_session_history()
            .first()
            .and_then(|&i| self.history_entry(i))
            .is_some_and(|e| e.text == entry && e.session_id == session_id);
        if already_latest_in_session {
            return;
        }
        let recorded = nuo_wire::HistoryEntry::new(entry, session_id, workspace, now);
        self.push_history(recorded.clone());
        // Same dedup guard as above: a backfilled row for this text is now
        // redundant with the recorded one.
        self.prune_backfill_after_record(&recorded.text);
        // `save_history` lock+merges into the on-disk union, so persisting just
        // the new entry is enough and cheap. Off-thread: the write takes a file
        // lock and must not block the event loop. Skipped entirely when disk
        // persistence is disabled (tests).
        if self.input_history_persist {
            self.send_intent(nuo_wire::AgentRequest::RecordInputHistory {
                entries: vec![recorded],
                dedup: false,
            });
        }
    }

    /// Drop the oldest cached attachment entries (FIFO) once the cache
    /// exceeds [`Self::HISTORY_ATTACHMENTS_CAP`]. `history_attachments_order`
    /// records first-seen order; a re-sent identity keeps its original slot.
    fn prune_history_attachments(&mut self) {
        while self.history_attachments.len() > Self::HISTORY_ATTACHMENTS_CAP {
            let Some(key) = self.history_attachments_order.pop_front() else {
                break;
            };
            self.history_attachments.remove(&key);
        }
    }

    /// Restore the attachments cached behind the history entry at
    /// `orig_idx` (an index into [`App::input_history`], as returned by
    /// `current_session_history` / `history_rows`) into the composer's
    /// `pending_images` / `pending_text_pastes`, or clear them when the
    /// entry has no cache (e.g. loaded from disk before this process
    /// recorded it). The recalled input text already carries the matching
    /// `[Image #N …]` / `[Pasted text #N …]` chips, so staging the payloads
    /// is all that is needed to re-arm a resend.
    pub fn restore_history_attachments(&mut self, orig_idx: usize) {
        let Some(entry) = self.history_entry(orig_idx) else {
            return;
        };
        let identity = (entry.text.clone(), entry.session_id.clone());
        match self.history_attachments.get(&identity) {
            Some(attachments) => {
                self.pending_images = attachments.images.clone();
                self.pending_text_pastes = attachments.text_pastes.clone();
            }
            None => {
                // No cached payloads: a fresh send must not inherit
                // attachments staged for some other entry, so clear them.
                self.pending_images.clear();
                self.pending_text_pastes.clear();
            }
        }
    }

    /// Load the history entry at `orig_idx` (an index from
    /// [`Self::current_session_history`] — spanning the persisted store and
    /// the session backfill) into the composer: its text, its cached
    /// attachments, cursor at the end, completion popup latched closed.
    /// Shared by the ↑/↓ walk and Ctrl+R insert so every recall path stays
    /// identical on the details.
    fn load_history_row(&mut self, orig_idx: usize) {
        let Some(entry) = self.history_entry(orig_idx) else {
            return;
        };
        self.input = entry.text.clone();
        self.set_cursor_end();
        self.restore_history_attachments(orig_idx);
        // History navigation is a programmatic input replacement, not an
        // edit — so it latches `completion_dismissed` like a slash-command
        // accept rather than re-enabling the popup the way InsertChar /
        // Backspace do. This keeps a recalled slash command from flashing
        // its completion menu until the next real keystroke clears the latch.
        self.suggestion_index = None;
        self.completion_dismissed = true;
    }

    /// Advance the inline ↑/↓ history cursor one step toward **older**
    /// entries (the ↑ key). `session_rows` is the newest-first index slice
    /// from [`App::current_session_history`], so position 0 is the newest
    /// entry and larger positions are older.
    ///
    /// The first ↑ stashes the in-progress draft — text and any staged
    /// attachments together — so a later ↓ past the newest entry restores
    /// it instead of leaving the composer empty. Subsequent ↑ walk further
    /// back until the oldest entry. Once at the oldest entry, further ↑
    /// do nothing (no-op) so the composer text and cursor remain intact
    /// rather than reloading and resetting the cursor. Returns `true`
    /// when a row was loaded; `false` when the slice is empty or already
    /// at the oldest entry.
    pub fn history_prev(&mut self, session_rows: &[usize]) -> bool {
        if session_rows.is_empty() {
            return false;
        }
        let new_pos = match self.history_index {
            Some(p) => {
                if p >= session_rows.len() - 1 {
                    return false;
                }
                p + 1
            }
            None => {
                // First ↑: stash the in-progress draft (and its staged
                // attachments) so a later ↓ past the newest entry restores
                // it instead of leaving the composer empty.
                self.history_draft = std::mem::take(&mut self.input);
                self.history_draft_images = std::mem::take(&mut self.pending_images);
                self.history_draft_text_pastes = std::mem::take(&mut self.pending_text_pastes);
                0
            }
        };
        self.history_index = Some(new_pos);
        self.load_history_row(session_rows[new_pos]);
        true
    }

    /// Move the inline history cursor one step toward **newer** entries
    /// (the ↓ key), mirroring [`App::history_prev`]. Walking past the
    /// newest entry (position 0) restores the draft stashed on the first ↑
    /// — text and attachments together. Returns `true` when a row was
    /// loaded; `false` when the cursor is already at the newest edge (or
    /// was never armed), in which case the draft has been restored.
    pub fn history_next(&mut self, session_rows: &[usize]) -> bool {
        let Some(pos) = self.history_index else {
            return false;
        };
        if pos == 0 {
            // Walked back to the newest entry: restore the draft the user
            // was composing before the first ↑ — text and any staged
            // attachments together — rather than blanking the composer.
            self.history_index = None;
            self.input = std::mem::take(&mut self.history_draft);
            self.pending_images = std::mem::take(&mut self.history_draft_images);
            self.pending_text_pastes = std::mem::take(&mut self.history_draft_text_pastes);
            self.set_cursor_end();
            // The restored draft may be a partial slash/path the user was
            // mid-edit on, but it still arrived via navigation rather than
            // a keystroke, so hold the latch until the next edit.
            self.suggestion_index = None;
            self.completion_dismissed = true;
            return false;
        }
        let new_pos = pos - 1;
        self.history_index = Some(new_pos);
        self.load_history_row(session_rows[new_pos]);
        true
    }

    /// Cancel inline history recall and restore the draft that was saved
    /// before navigation began.
    pub fn cancel_history_recall(&mut self) {
        if self.history_index.is_some() {
            self.history_index = None;
            self.input = std::mem::take(&mut self.history_draft);
            self.pending_images = std::mem::take(&mut self.history_draft_images);
            self.pending_text_pastes = std::mem::take(&mut self.history_draft_text_pastes);
            self.set_cursor_end();
            self.suggestion_index = None;
            self.completion_dismissed = true;
        }
    }

    /// Delete an entry from input history at `orig_idx` (an index into [`App::input_history`]),
    /// cascading to SQLite persistence, session backfill, and attachment cache.
    pub fn delete_history_entry_at(
        &mut self,
        orig_idx: usize,
    ) -> Option<nuo_wire::HistoryEntry> {
        if orig_idx >= self.input_history.len() {
            return None;
        }
        let removed = self.input_history.remove(orig_idx);
        // Cascade 1: Prune matching text from current session's backfill
        self.session_history_backfill
            .retain(|e| e.text != removed.text);
        // Cascade 2: Remove cached attachments
        let identity = (removed.text.clone(), removed.session_id.clone());
        self.history_attachments.remove(&identity);
        self.history_attachments_order.retain(|k| k != &identity);
        // Cascade 3: invalidate the server's on-disk record.
        if self.input_history_persist {
            self.send_intent(nuo_wire::AgentRequest::DeleteInputHistoryEntry {
                text: removed.text.clone(),
                created_at_ms: removed.created_at_ms,
            });
        }
        Some(removed)
    }

    /// Delete the currently selected entry in the Ctrl+R history panel, adjusting
    /// selection and follow states.
    pub fn delete_selected_history_entry(&mut self) -> Option<nuo_wire::HistoryEntry> {
        let ranked = self.history_rows();
        let pick = ranked
            .get(self.surfaces.dlg_mut::<crate::surfaces::HistorySearchDialog>().index)
            .or_else(|| ranked.first());
        let &(orig_idx, _) = pick?;
        let removed = self.delete_history_entry_at(orig_idx);
        let new_len = self.history_rows().len();
        if self.surfaces.dlg_mut::<crate::surfaces::HistorySearchDialog>().index >= new_len {
            self.surfaces.dlg_mut::<crate::surfaces::HistorySearchDialog>().index = new_len.saturating_sub(1);
        }
        self.surfaces.dlg_mut::<crate::surfaces::HistorySearchDialog>().follow = true;
        removed
    }
}
