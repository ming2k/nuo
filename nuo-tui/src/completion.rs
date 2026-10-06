//! Client-side presentation adapter for daemon-owned composer completion.
//!
//! Matching, intent steering, project scanning, and path resolution happen in
//! Nuo. The TUI only requests results, translates wire offsets into Rust byte
//! offsets, and renders/applies the returned edits.

use crate::App;
use crate::composer::{composer_text_width, composer_wrapped_pos};
use crate::design::COMPOSER_PROMPT_PREFIX_COLS;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandDoc {
    pub name: String,
    /// The single prose introduction (contract has exactly one field).
    pub summary: String,
    pub usage: Vec<String>,
    pub category: Option<String>,
    /// First-token verbs (`/schedule list` → `list`) with their own
    /// introductions; rendered after the parent's usage block.
    pub subcommands: Vec<(String, String)>,
}

impl CommandDoc {
    pub fn from_spec(spec: &nuo_wire::CommandSpec) -> Self {
        Self {
            name: spec.name.clone(),
            summary: spec.summary.clone(),
            usage: spec.usage.clone(),
            category: spec.category.clone(),
            subcommands: spec
                .subcommands
                .iter()
                .map(|sub| (sub.name.clone(), sub.summary.clone()))
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompletionKind {
    #[default]
    None,
    Slash,
    Path,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CompletionItemKind {
    #[default]
    Slash,
    SlashAlias,
    IntentSuggestion {
        matched_intent: String,
        reason: String,
    },
    PathFile,
    PathDir,
    PathExplicit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub label: String,
    pub description: String,
    pub insert_text: String,
    pub replace_start: usize,
    pub replace_end: usize,
    pub kind: CompletionItemKind,
    /// Canonical command when this row is an alias (`/yolo` → `/unattended`).
    /// Drives the distinct row rendering; the accepted edit already commits
    /// the canonical spelling via `insert_text`.
    pub alias_of: Option<String>,
    pub doc: Option<CommandDoc>,
}

impl Completion {
    pub fn whole_input(label: &str, description: &str, input_len: usize) -> Self {
        Self {
            label: label.to_string(),
            description: description.to_string(),
            insert_text: label.to_string(),
            replace_start: 0,
            replace_end: input_len,
            kind: CompletionItemKind::Slash,
            alias_of: None,
            doc: None,
        }
    }

    fn from_backend(input: &str, item: &nuo_wire::InputCompletion) -> Option<Self> {
        let replace_start = char_to_byte(input, item.replace_start)?;
        let replace_end = char_to_byte(input, item.replace_end)?;
        if replace_start > replace_end {
            return None;
        }
        let kind = match item.kind {
            nuo_wire::InputCompletionKind::Slash => CompletionItemKind::Slash,
            nuo_wire::InputCompletionKind::SlashAlias => CompletionItemKind::SlashAlias,
            nuo_wire::InputCompletionKind::Intent => CompletionItemKind::IntentSuggestion {
                matched_intent: String::new(),
                reason: item.description.clone(),
            },
            nuo_wire::InputCompletionKind::PathFile => CompletionItemKind::PathFile,
            nuo_wire::InputCompletionKind::PathDir => CompletionItemKind::PathDir,
            nuo_wire::InputCompletionKind::PathExplicit => CompletionItemKind::PathExplicit,
        };
        Some(Self {
            label: item.label.clone(),
            description: item.description.clone(),
            insert_text: item.insert_text.clone(),
            replace_start,
            replace_end,
            kind,
            alias_of: item.alias_of.clone(),
            doc: item.command.as_ref().map(CommandDoc::from_spec),
        })
    }
}

fn char_to_byte(input: &str, char_index: usize) -> Option<usize> {
    if char_index == input.chars().count() {
        Some(input.len())
    } else {
        input.char_indices().nth(char_index).map(|(byte, _)| byte)
    }
}

pub fn completion_anchor(
    input: &str,
    byte_cursor: usize,
    input_rect: nuotc::Rect,
    input_scroll: usize,
    kind: CompletionKind,
) -> nuotc::Rect {
    let text_width = composer_text_width(input_rect.width as usize);
    let trigger_byte = match kind {
        CompletionKind::Path => mention_range_at(input, byte_cursor)
            .map(|(start, _)| start)
            .unwrap_or(0),
        _ => 0,
    };
    let (row, col) = composer_wrapped_pos(input, text_width, trigger_byte);
    let x = input_rect.x + COMPOSER_PROMPT_PREFIX_COLS as u16 + col.min(text_width) as u16;

    let visible_rows = (input_rect.height as usize)
        .saturating_sub(crate::design::COMPOSER_VERTICAL_CHROME_ROWS as usize)
        .max(1);

    let anchor_y = if row <= input_scroll {
        input_rect.y
    } else {
        let visible_row = (row - input_scroll).min(visible_rows.saturating_sub(1));
        input_rect.y + crate::design::COMPOSER_TEXT_ROW_OFFSET + visible_row as u16
    };

    nuotc::Rect::new(x, anchor_y, 1, 1)
}

pub fn completion_anchor_x(
    input: &str,
    byte_cursor: usize,
    input_rect: nuotc::Rect,
    kind: CompletionKind,
) -> u16 {
    completion_anchor(input, byte_cursor, input_rect, 0, kind).x
}

pub fn resolved_slash_command_len(
    input: &str,
    catalog: &nuo_wire::CommandCatalog,
) -> Option<usize> {
    if !input.starts_with('/') {
        return None;
    }
    let token = input
        .trim()
        .split_once(char::is_whitespace)
        .map(|(name, _)| name)
        .unwrap_or_else(|| input.trim());
    (token.len() > 1 && catalog.recognizes(token)).then_some(token.len())
}

pub(super) fn mention_range_at(input: &str, cursor_byte: usize) -> Option<(usize, usize)> {
    nuo_wire::mention::mention_range_at(input, cursor_byte)
}

impl App {
    pub fn completion_kind(&self) -> CompletionKind {
        if self.input.starts_with('/') {
            CompletionKind::Slash
        } else if self.active_mention_range().is_some() {
            CompletionKind::Path
        } else {
            CompletionKind::None
        }
    }

    pub fn completion_trigger_text_present(&self) -> bool {
        match self.completion_kind() {
            CompletionKind::None => false,
            CompletionKind::Slash => {
                !self.input.trim().is_empty() && !self.known_exact_slash_input()
            }
            CompletionKind::Path => true,
        }
    }

    fn known_exact_slash_input(&self) -> bool {
        let Some(first) = self.input.split_whitespace().next() else {
            return false;
        };
        self.input.starts_with('/')
            && self.input.trim() == first
            && self.command_catalog.recognizes(first)
    }

    pub fn anchor_completion_selection(&mut self, completions: &[Completion]) {
        let input_len = self.input.len();
        let exact = completions.iter().any(|item| {
            item.replace_start == 0 && item.replace_end == input_len && item.label == self.input
        });
        let visible = !completions.is_empty() && !exact;
        match (visible, self.suggestion_index) {
            (false, _) => self.suggestion_index = None,
            (true, None) => self.suggestion_index = Some(0),
            (true, Some(index)) => {
                self.suggestion_index = Some(index.min(completions.len() - 1));
            }
        }
    }

    /// Current completions translated into renderer-native byte edits.
    ///
    /// Implements Two-Tier Completion (ADR-0162):
    /// - Tier 1: Zero-latency synchronous execution for slash and harness commands.
    /// - Tier 2: Path mentions and dynamic queries with SWR (Stale-While-Revalidate) cache retention.
    pub fn completions(&self) -> Vec<Completion> {
        let cursor = self.cursor_position;

        // Tier 1: Synchronous zero-latency matching for slash and harness commands (ADR-0162)
        if self.input.starts_with('/') {
            let cursor_byte = char_to_byte(&self.input, cursor).unwrap_or(self.input.len());
            let items =
                nuo_client::complete_slash_items(&self.command_catalog, &self.input, cursor_byte);
            return items
                .iter()
                .filter_map(|item| Completion::from_backend(&self.input, item))
                .collect();
        }

        // Tier 2: Path mentions and dynamic queries from backend (SWR with cache retention)
        let items = if self.completion_response_input.as_deref() == Some(self.input.as_str())
            && self.completion_response_cursor == cursor
        {
            self.backend_completions.clone()
        } else if self.active_mention_range().is_some() && !self.backend_completions.is_empty() {
            // SWR: Retain active backend completions while in-flight
            self.backend_completions.clone()
        } else {
            #[cfg(test)]
            {
                nuo_client::complete_for_frontend_test(
                    self.command_catalog.clone(),
                    self.cwd.clone(),
                    &self.input,
                    cursor,
                )
            }
            #[cfg(not(test))]
            {
                Vec::new()
            }
        };

        items
            .iter()
            .filter_map(|item| Completion::from_backend(&self.input, item))
            .collect()
    }

    /// Send a completion request when the composer state changed.
    pub fn refresh_backend_completion_request(&mut self) {
        let cursor = self.cursor_position;
        let state = (self.input.clone(), cursor);
        if self.completion_requested.as_ref() == Some(&state) {
            return;
        }
        self.completion_requested = Some(state.clone());

        // When not inside an active mention, clean up stale backend completions.
        // Slash commands are handled synchronously by Tier 1 with zero latency (ADR-0162).
        if !self.input.starts_with('/') && self.active_mention_range().is_none() {
            self.backend_completions.clear();
            self.completion_response_input = None;
            self.completion_response_cursor = 0;
            return;
        }

        // Retain `backend_completions` during typing (ADR-0162 SWR).
        // Only bump generation request ID and send request if dynamic path completion is needed.
        if self.active_mention_range().is_some() {
            self.completion_request_id = self.completion_request_id.wrapping_add(1);
            self.send_intent(nuo_wire::AgentRequest::CompleteComposer {
                request_id: self.completion_request_id,
                text: state.0,
                cursor,
            });
        }
    }

    pub fn apply_backend_completions(
        &mut self,
        request_id: u64,
        input: String,
        cursor: usize,
        items: Vec<nuo_wire::InputCompletion>,
    ) {
        if request_id != self.completion_request_id
            || input != self.input
            || cursor != self.cursor_position
        {
            return;
        }
        self.completion_response_input = Some(input);
        self.completion_response_cursor = cursor;
        self.backend_completions = items;
    }

    pub fn active_mention_range(&self) -> Option<(usize, usize)> {
        mention_range_at(&self.input, self.byte_cursor())
    }
}
