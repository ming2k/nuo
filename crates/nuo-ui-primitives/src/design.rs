//! Non-color design tokens: spacing, gutters, fixed row counts, and text
//! measurement limits shared by renderer components.
/// Uniform horizontal inset applied to transcript-area components so bands,
/// bars, and text do not touch the terminal frame.
pub const TRANSCRIPT_H_INSET: u16 = 2;

/// Extra leading whitespace applied to prose after the transcript-area gutter.
/// Now that the horizontal gutter is applied once at the stream entry point,
/// this is the *only* indent prose-like content adds on top of the already-
/// inset rect.
pub const TRANSCRIPT_BODY_LEADING_INDENT: u16 = 2;

// Command card
// A command row is a *card*, not flat prose: it paints a full-width band
// (`Theme::command_surface`) with a thick `┃` identity bar in the family
// tone — the same card grammar the user-message panel, the code band, and
// the notice card already speak. The geometry tokens below are that card's
// shared contract, so every phase/layout of the command component renders
// inside the same frame.

/// Columns of card chrome before the disclosure marker / glyph: the `┃`
/// identity bar (1) plus one gutter column (1). The marker column is
/// reserved *inside* the card at a fixed offset for every phase — pending
/// rows render a blank there — so a row never shifts horizontally when its
/// reply settles or its layout class changes.
pub const COMMAND_CARD_LEAD_COLS: usize = 2;

/// Minimum readable width for compact expandable step header rows.
pub const STEP_MIN_WIDTH: usize = 8;

/// Inner indent for expanded tool-step body content. The transcript band already
/// carries the outer gutter; this indent aligns label-free tool details with
/// prose-like body content inside that band.
pub const TOOL_STEP_BODY_INDENT_COLS: usize = 2;

/// One blank row inserted at semantic transcript, turn, or component-segment
/// boundaries. Consecutive tool-like components in one known turn are the
/// compact exception and use zero rows.
pub const MESSAGE_GAP_ROWS: usize = 1;

/// Separation between a turn metadata header and its first component. The
/// header labels the group but is not part of the component stack, so one row
/// preserves the hierarchy without giving every child its own top margin.
pub const TURN_HEADER_BODY_GAP_ROWS: usize = 1;

/// Blank surface gap rows between a user message header (`› round N · HH:MM`)
/// and its message body panel.
pub const USER_MESSAGE_HEADER_BODY_GAP_ROWS: usize = 1;

/// Lead glyph for sent user message headers. Aliased to the canonical
/// [`INPUT_PROMPT_GLYPH`] so a sent message carries the same `›` as the composer
/// that produced it. The former ASCII `<` (and its "Unix stdin redirection"
/// rationale) is retired in favour of the project's canonical single guillemet.
pub const USER_MESSAGE_GUTTER_GLYPH: &str = INPUT_PROMPT_GLYPH;

/// Lead glyph for assistant turn headers. Aliased to the canonical
/// [`INPUT_PROMPT_GLYPH`], retiring the former ASCII `>` (and its "Unix stdout
/// redirection" rationale) so no lead drifts to a bare ASCII arrow again.
pub const AI_OUTPUT_LEAD_GLYPH: &str = INPUT_PROMPT_GLYPH;

/// Blockquote gutter glyph for transcript prose (`Block::Quote`): a light
/// vertical rule that marks the quoted run. Painted on every wrapped row so
/// the quote reads as one continuous block, at a fixed 5-column lead
/// (`"   ▎ "`). Kept as a named token so the prose, the web-article payload
/// renderer, and the docs can never drift to different glyphs again.
pub const QUOTE_GUTTER_GLYPH: &str = "▎";

/// Columns occupied by the blockquote lead: the [`TRANSCRIPT_BODY_LEADING_INDENT`]
/// indent (2) + the [`QUOTE_GUTTER_GLYPH`] (1) + one trailing gap (1) + one
/// more leading pad column (1) = 5. Both the wrap budget and the recorded
/// `prefix_cols` for hit-testing derive from this single value.
pub const QUOTE_PREFIX_COLS: usize = 5;

/// The blockquote lead as a literal run of [`QUOTE_PREFIX_COLS`] columns, so
/// the rendered width and [`QUOTE_PREFIX_COLS`] can never disagree.
pub const QUOTE_PREFIX: &str = "   ▎ ";

/// Lead glyph on an answered question's selected-option row inside an expanded
/// `ask_user` step. The rightward hook visually ties each answer back to the
/// question above it, so a multi-question request reads as a Q→A list rather
/// than two independent columns of text. Only the *first* selected option of a
/// question carries it; further options align under the label column so a
/// multi-select answer reads as one block.
pub const QUESTION_ANSWER_GLYPH: &str = "↳";

/// Columns between the answer glyph and the selected label text.
pub const QUESTION_ANSWER_GAP_COLS: usize = 1;

/// Separator between a question's short `header` chip, its text, and its
/// option-count meta on one rendered line (`Scope · … (2 options)`).
pub const QUESTION_META_SEPARATOR: &str = " · ";

/// Vertical chrome rows around a sent user message panel: one top transition
/// row and one bottom transition row.
pub const USER_MESSAGE_TRANSITION_ROWS: usize = 1;

/// Vertical gap between an expanded tool step's header and its own body. Held
/// to **0**: a tool step is a flat *log entry* (no band, no Tool/Arguments/
/// Result labels — see `draw_tool_step`), so the body's grouping under its
/// header is carried by a **single** signal — the indent
/// (`TOOL_STEP_BODY_INDENT_COLS`). A blank row here would be a *panel/card*
/// affordance left over from the old banded shape; it competes with the indent
/// rather than reinforcing it (the row says "two separate blocks", the indent
/// says "this is the header's content"). Removing it lets the indent own the
/// grouping and keeps an expanded step as tight as a collapsed batch.
///
/// The token is kept at 0 rather than deleted so the decision is *visible* in
/// code: the absence of a top gap is a deliberate choice, not an oversight,
/// and the one place that would want to re-introduce it (`draw_tool_step`)
/// reads the named token.
///
/// The layout closes the body only when the next component crosses a semantic
/// turn/message boundary. There is no dedicated bottom-gap token: an extra
/// one would break the flush same-turn tool batch.
pub const TOOL_STEP_BODY_TOP_GAP_ROWS: usize = 0;
pub const TOOL_STEP_SECTION_GAP_ROWS: usize = 1;
pub const TOOL_STEP_CHILDREN_GAP_ROWS: usize = TOOL_STEP_SECTION_GAP_ROWS;

/// Spacing inside expanded reasoning traces. The first body line sits directly
/// below the disclosure header; later reasoning blocks retain one row of
/// separation. These stay independent from tool-step spacing because reasoning
/// is prose-like, not a panel.
/// There is no bottom-gap token: the layout resolves the following semantic
/// component boundary.
pub const REASONING_TRACE_BODY_TOP_GAP_ROWS: usize = 0;
pub const REASONING_TRACE_BLOCK_GAP_ROWS: usize = 1;

/// Gap rows between the bottom of the composer panel and the bar below it.
/// The panel's hint row (carrying the Enter/Tab sentence + char count) reads
/// as built-in separation — an extra `surface` gap row on top of that just
/// burns a transcript row for no visual gain, so the next bar sits flush
/// against the panel's bottom edge.
pub const COMPOSER_HINT_GAP_ROWS: u16 = 0;

/// Gap rows between the activity bar and the top of the composer panel.
/// Mirrors [`COMPOSER_HINT_GAP_ROWS`] on the upper edge: the panel's top
/// padding row already separates its text from the live status line, so the
/// activity bar sits flush against the composer with zero extra breathing
/// room. Test-only since the footer stack made the zero structural
/// (adjacent rows place flush by construction in `footer_stack::place`); the
/// token stays as the recorded decision, asserted by `footer_stack`'s tests.
pub const ACTIVITY_COMPOSER_GAP_ROWS: u16 = 0;

/// Model bar: a single-line strip pinned directly below the input box that
/// surfaces the ambient gauges — model identity, context usage, stream rate.
/// The Enter-action keys and the `as:` target row live inside the composer's
/// own padding rows. It intentionally does **not** carry long-lived session
/// state — that lives on the head row at the top of the view. Always one row
/// tall when visible (hidden only while an overlay modal replaces the chrome).
pub const MODEL_BAR_ROWS: u16 = 1;
/// Edge indent of model-bar content: the gauge cluster leads with it on
/// the left and the identity cluster trails with it on the right, so the
/// justified halves mirror each other (matching the composer's prompt
/// prefix feel).
pub const MODEL_BAR_INNER_PADDING: usize = 1;
/// Minimum gap between the left-anchored gauge cluster (context usage)
/// and the right-pinned model-identity cluster.
pub const MODEL_BAR_GAP_MIN: usize = 2;
/// Gap *inside* the model-identity group (`model effort @instance`) — these
/// three tokens read as one identity, so they sit tighter than the gap
/// between the identity group and the context-usage segment.
pub const MODEL_BAR_MODEL_GAP: usize = 1;

/// ActivityStrip: the consolidated single-row strip (ADR-0062 [INV-FOOTER-02])
/// shown directly above Composer while a round, queue items, or tasks are active.
pub const ACTIVITY_STRIP_ROWS: u16 = 1;
/// Minimum gap between a footer bar's left content and its right-pinned
/// keycap legend (e.g. the queue bar's `Ctrl+Q expand`). Deliberately wider than the
/// 2-col inter-cluster
/// gap used by the hint bar: a legend is a keyboard affordance, not
/// prose, so it needs real visual distance from the content — especially when
/// content truncates to fill the row, where a small gap would let a `…` butt
/// directly against a keycap.
#[allow(dead_code)]
pub const BAR_LEGEND_GAP_MIN: usize = 6;

// Semantic joins (see docs/reference/tui/visual-language.md)
// Keep labels free of punctuation soup. Atomic values use ordinary spaces,
// peer metadata uses a two-column gap, secondary measures use parentheses,
// cause/reason uses an em dash, and hierarchy uses a breadcrumb.
/// Same-rank peer enumeration — pure whitespace, no glyph (columns).
pub const JOIN_ENUMERATE_COLS: usize = 2;
/// Container › member breadcrumb for inline hierarchy (`round 3 › turn 2`).
#[allow(dead_code)]
pub const JOIN_BREADCRUMB: &str = " › ";
/// Cause that explains a state, joined with an em dash (`terminated — timed out`).
/// This is the *only* form for a cause: parentheses mean a secondary measure
/// (`4.2k (7%)`), so `state (cause)` would misdeclare the relationship. Spaced
/// exactly as the docs table shows it, so call sites never re-invent the
/// spacing.
pub const JOIN_CAUSE: &str = " — ";
/// Queue bar: a one-line persistent region pinned in the footer stack
/// (above the transient activity bar) that always surfaces the pending
/// outbox (the `QUEUE` identity + count, an inline preview of the next item
/// to pop, and the key affordances). Always one row tall when visible
/// (hidden only while an overlay modal replaces the chrome, inside a subagent
/// zoom, or when the outbox is empty). It is the permanent home for queue
/// affordances, so the hint bar no longer needs to embed outbox counts.
/// Rendered on the plain surface (no raised tint, no glyph) so it stays
/// quiet, matching the todo bar above it.
#[allow(dead_code)]
pub const QUEUE_BAR_ROWS: u16 = 1;

/// Minimum display columns the workspace path may claim before the bar stays
/// silent about it.
///
/// The floor is measured, not guessed: the `PathView` ladder's last stop is the
/// bare leaf (`adaptive_shorten("~/projects/nuo", 4) == "nuo"`), so four columns
/// still name the project honestly. Below that the ladder has nothing faithful
/// left to say, and a path fragment would assert a location it cannot show.
/// Keeping the floor this low matters — an earlier value of 8 discarded widths
/// 4..=7, which the ladder serves correctly, wasting usable columns on a row
/// where the path is otherwise the only optional element.
pub const CONTEXT_BAR_WORKSPACE_MIN_COLS: usize = 4;

/// Blank rows inserted at the start of the transcript message stream
/// (scroll padding top). When scrolled to the very top (`scroll = 0`),
/// this provides 1 row of visual separation below the page header.
pub const STREAM_TOP_GAP_ROWS: usize = 1;

/// Blank rows inserted at the end of the transcript message stream
/// (scroll padding bottom). When scrolled to the very bottom (`scroll = max_scroll`),
/// this provides 1 row of visual separation above the footer chrome.
pub const STREAM_BOTTOM_GAP_ROWS: usize = 1;

/// Permanent breathing room between the transcript and footer chrome.
/// Set to 0 because the content stream now owns its own scroll padding
/// via [`STREAM_TOP_GAP_ROWS`] and [`STREAM_BOTTOM_GAP_ROWS`].
pub const FOOTER_TOP_GAP_ROWS: u16 = 0;
/// Maximum height of the head band shown at the top of every transcript
/// page — Main (thread), Subagent, Dashboard, and Settings all share this
/// single chrome slot. Under ADR-0062 it is structured across a polymorphic hierarchy:
/// - Tier 1: Client Global TabBar (`draw_tab_bar`)
/// - Tier 2: Polymorphic SceneBar (`draw_scene_bar`)
/// - Tier 3: Sovereign ThreadContextBar (`draw_thread_context_bar`, 0 rows on non-thread scenes)
pub const PAGE_HEADER_ROWS: u16 = 3;

/// Horizontal inset applied to the footer area containing status/composer/hints.
pub const FOOTER_H_INSET: u16 = TRANSCRIPT_H_INSET;

/// Composer chrome is the panel's own breathing room: one blank padding row
/// above the text, one blank gap row below it, and one meta row carrying the
/// hint sentence (`Enter send prompt · 12 chars`) at the bottom — four rows
/// total for a one-line draft. The box reads as a tinted surface, not a
/// lined frame.
pub const COMPOSER_VERTICAL_CHROME_ROWS: u16 = 3;
pub const COMPOSER_MIN_HEIGHT: u16 = 4;
pub const COMPOSER_MAX_HEIGHT_DIVISOR: u16 = 2;

/// The maximum height an interaction sheet (question / permission /
/// input-injection) may grow to when it replaces the composer slot. It is
/// the *composer's own* max-height rule — `terminal_height / 2` — so a
/// sheet opens with the same footprint the input box would have at its
/// largest, never covering more than half the terminal. The question sheet
/// may still grow further up to the terminal top when its content demands
/// it (a long question / many options), but this token is the "comfortable"
/// cap the sheet prefers to stay within; composer, permission, and question
/// all read the same formula so the slot never has two different ideas of
/// "how big can this get".
pub fn sheet_max_height(terminal_height: u16) -> u16 {
    (terminal_height / COMPOSER_MAX_HEIGHT_DIVISOR).max(COMPOSER_MIN_HEIGHT)
}
/// The canonical **lead glyph**: a single right-pointing angle quote (`›`,
/// U+203A — one column wide) that fronts every prompt and message lead in the
/// TUI — the composer, the command-palette query, the permission input-injection
/// field, the question sheet's "Other" field, the dashboard console, the provider
/// search rows, the completion menu's suggestions, and the transcript's
/// user-message and assistant-turn headers ([`USER_MESSAGE_GUTTER_GLYPH`],
/// [`AI_OUTPUT_LEAD_GLYPH`]). A named token so no lead can drift to a bare ASCII
/// `<`/`>` again.
pub const INPUT_PROMPT_GLYPH: &str = "›";

/// The input prompt prefix: [`INPUT_PROMPT_GLYPH`] plus one gap column, a
/// literal run of [`COMPOSER_PROMPT_PREFIX_COLS`] columns. Surfaces that paint
/// the prefix as a single styled run (command palette, permission input
/// injection) use this so their rendered width and the composer's reserved
/// [`COMPOSER_PROMPT_PREFIX_COLS`] can never disagree.
pub const INPUT_PROMPT_PREFIX: &str = "› ";

/// Columns reserved before the composer text: the [`INPUT_PROMPT_GLYPH`] prompt
/// glyph plus one gap column on the first wrapped line; continuation lines
/// indent the same amount so the caret stays aligned. Text starts at column 2
/// of the box.
pub const COMPOSER_PROMPT_PREFIX_COLS: usize = 2;
pub const COMPOSER_TEXT_ROW_OFFSET: u16 = 1;

/// User message panels used to reserve their own outer gutter matching
/// [`TRANSCRIPT_H_INSET`]. Now that the horizontal inset is applied once at
/// the stream entry point (`draw_transcript` → `band`), the outer gutter is
/// redundant: the `band` rect already excludes it. Set to 0 so the panel
/// background starts at the band edge, with only the inner text gap remaining.
pub const USER_MESSAGE_OUTER_GUTTER_COLS: usize = 0;
/// Inner left padding (in `user_panel_bg`) between the outer gutter and the
/// text. Matches the composer's prompt prefix so sent messages and the input
/// box share the same left margin.
pub const USER_MESSAGE_TEXT_GAP_COLS: usize = 2;
/// Inner right padding (in `user_panel_bg`) kept clear of wrapped text so a
/// sent message never runs its text into the panel's right edge.
pub const USER_MESSAGE_RIGHT_PAD_COLS: usize = 2;

/// Inner right padding kept clear of wrapped text inside the composer so
/// typing never runs into the panel's right edge. Two columns of air between
/// the last glyph and the edge of the tinted box.
pub const COMPOSER_RIGHT_PAD_COLS: usize = 2;

// Modal overlays
// Every centered modal (Activity, Sessions, Provider, Help, …) goes through
// `modal_frame`, which paints a borderless solid-bg panel and splits it into
// header / body / footer. These tokens are the single source of truth for
// spacing *inside* that panel: header and body content flush-align to the
// inner area (`MODAL_INNER_H_PADDING`), maximizing usable horizontal space
// across every overlay.

/// Left/right padding between the panel edge and the header/body/footer.
/// Applied once by `modal_frame` via `Margin { horizontal, .. }`; section
/// content never adds its own outer gutter on top of this. Includes room for
/// the scrollbar track (1 col) plus `SCROLLBAR_GAP` (1 col) on the right.
pub const MODAL_INNER_H_PADDING: u16 = 3;

/// Empty columns between the body text's right edge and the scrollbar track.
pub const SCROLLBAR_GAP: u16 = 1;

/// Top/bottom padding between the panel edge and the header/body/footer.
/// Applied once by `modal_frame` via `Margin { vertical, .. }`.
pub const MODAL_INNER_V_PADDING: u16 = 1;

// Block-level code/text surfaces
// Every block-level content surface — the markdown `Block::Code` band and the
// tool-step result blocks (read / bash / listing / matches / diff) — shares ONE
// design contract so a code block looks the same whether it sits in assistant
// prose or inside an expanded tool step. These tokens are that contract: the
// geometry every code band agrees on. Colors live in `theme` (incl. the diff
// tokens); spacing/gutter live here.

/// Horizontal inset of block-level solid surfaces from the already-inset
/// transcript band. This keeps markdown code/math bands visually panel-like
/// without re-applying the global transcript gutter.
pub const BLOCK_SURFACE_H_INSET: u16 = 2;

/// Columns between the math marker glyph and the rendered math text.
pub const MATH_MARKER_GAP_COLS: usize = 2;

/// Columns of left padding inside a code band before the line-number gutter.
/// Markdown and tool-step bands use the same inner left margin so the code
/// text column lines up across block origins.
pub const CODE_BAND_LEFT_INDENT: usize = 2;

/// Empty columns between the line-number gutter and the code text. Wide enough
/// to read the gutter as a distinct column, narrow enough not to waste width.
pub const CODE_BAND_GUTTER_GAP: usize = 1;

/// Minimum width of the line-number column so single-digit files align
/// cleanly. Grows to fit the highest displayed line number in either band.
pub const CODE_BAND_GUTTER_MIN_WIDTH: usize = 2;

// Bash output middle-folding (ADR-0115).
//
// An expanded command step can emit hundreds of stdout/stderr lines, burying
// the trailing "events" — the `exit N` line and the termination footer
// (timeout / blocked / cancelled) — far below the fold. Folding collapses the
// verbose middle into a three-row negative-space gap (blank / `N lines
// omitted` / blank), keeping a head of leading context and a tail of trailing
// context plus every event footer always visible.
//
// The window is deliberately **tail-heavy**, mirroring the model-facing
// `[INV-FOLD-01]` 25/75 bias in `nuo-tool`: a command's setup and invocation
// context is short and mostly self-evident (the `$ command` row above the
// output already frames it), while the error, panic, and final status lines
// that explain the outcome live at the tail.
//
// The geometry is closed: folding is only permitted when it renders *fewer*
// rows than the verbatim body, which [`bash_fold_applies`] decides from the
// same three constants the renderer paints with. That makes the threshold
// impossible to drift from the chrome it accounts for.

/// Leading output lines kept visible above a folded bash middle.
pub const BASH_FOLD_HEAD_ROWS: usize = 2;
/// Trailing output lines kept visible below a folded bash middle.
pub const BASH_FOLD_TAIL_ROWS: usize = 6;
/// Rows the fold's negative-space gap occupies (blank / label / blank).
pub const BASH_FOLD_CHROME_ROWS: usize = 3;
/// Row count at or below which an expanded body renders verbatim — the
/// smallest body for which folding renders strictly fewer rows.
pub const BASH_FOLD_MIN_VERBATIM_ROWS: usize =
    BASH_FOLD_HEAD_ROWS + BASH_FOLD_CHROME_ROWS + BASH_FOLD_TAIL_ROWS;

// The bias is the point of the window: if this ever inverts, the fold would
// hide the outcome it exists to preserve. Compile-time, so no test run is
// needed for the guarantee to hold.
const _: () = assert!(BASH_FOLD_TAIL_ROWS > BASH_FOLD_HEAD_ROWS);
const _: () = assert!(BASH_FOLD_TAIL_ROWS >= 2 * BASH_FOLD_HEAD_ROWS);
const _: () = assert!(BASH_FOLD_HEAD_ROWS > 0, "some leading context must survive");

/// Whether a command step's expanded body with `logical_rows` output rows folds
/// its middle. Folding must always *save* rows: at `BASH_FOLD_MIN_VERBATIM_ROWS`
/// the folded body would be exactly as tall as the verbatim one, so it stays
/// verbatim (nothing hidden for no gain).
pub const fn bash_fold_applies(logical_rows: usize) -> bool {
    logical_rows > BASH_FOLD_MIN_VERBATIM_ROWS
}

// Minimum terminal size
// Below this geometry the layout math (footer split, composer height, code
// band gutters) would underflow or produce an unusable UI. Instead of drawing
// garbage — or panicking deep in a subtraction chain — `draw_transcript`
// short-circuits and shows a single centered notice telling the user how large
// the terminal must be, hiding everything else. The values are the smallest
// width/height at which the normal footer (status bar + composer + hint bar)
// plus a one-line transcript row all fit.

/// Minimum terminal width (columns) for a usable layout. 44 is the width at
/// which the built-in 42-column wordmark fits with one column of margin on
/// each side, so the empty-state hero never has to wrap or truncate the logo
/// to fit the narrowest supported terminal (see the empty-state module). The
/// footer keeps its own room: the horizontal inset on both sides plus the
/// composer prompt prefix and right pad.
pub const MIN_TERMINAL_COLS: u16 = 44;

/// Minimum terminal height (rows) for a usable layout. Accounts for the
/// viewport's top margin (1 row; the bottom margin is 0 — the hint bar pins
/// flush to the terminal's bottom edge), the minimum footer chrome (the
/// permanent transcript/footer gap, the composer minimum, and the hint bar —
/// the activity bar and queue bar appear only while active/pending), and at
/// least a couple of transcript rows. Below this the hero degrades to the
/// carousel line alone rather than clipping the wordmark.
pub const MIN_TERMINAL_ROWS: u16 = 12;

/// Whether a terminal geometry is below the usable minimum. This is the one
/// authority for the "terminal too small" state: the renderer uses it to swap
/// the whole UI for the notice, and the event loop uses it to freeze input and
/// animation so nothing changes behind the notice (see the event loop).
pub fn below_minimum(width: u16, height: u16) -> bool {
    width < MIN_TERMINAL_COLS || height < MIN_TERMINAL_ROWS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_prompt_prefix_matches_composer_reservation() {
        // Every input surface starts its text at the same column: the shared
        // prompt prefix must lead with the canonical glyph and occupy exactly
        // the columns the composer reserves.
        assert_eq!(
            INPUT_PROMPT_GLYPH, "›",
            "the input prompt lead must be the single guillemet ›",
        );
        assert!(
            INPUT_PROMPT_PREFIX.starts_with(INPUT_PROMPT_GLYPH),
            "the prefix must lead with the prompt glyph",
        );
        assert_eq!(INPUT_PROMPT_GLYPH.chars().count(), 1);
        assert_eq!(
            INPUT_PROMPT_PREFIX.chars().count(),
            COMPOSER_PROMPT_PREFIX_COLS,
        );
    }

    #[test]
    fn every_lead_uses_the_canonical_glyph() {
        // One lead everywhere: the transcript headers must render the same single
        // guillemet as the input surfaces, so no ASCII `<`/`>` can reappear.
        assert_eq!(USER_MESSAGE_GUTTER_GLYPH, INPUT_PROMPT_GLYPH);
        assert_eq!(AI_OUTPUT_LEAD_GLYPH, INPUT_PROMPT_GLYPH);
    }

    /// Folding is a *compaction*: it must never render a body taller than the
    /// verbatim one. This is the invariant the pre-ADR-0115 threshold violated
    /// (an 8-row body folded to HEAD + 3 chrome + TAIL = 9 rows).
    #[test]
    fn bash_fold_only_applies_when_it_saves_rows() {
        let folded_rows = |rows: usize| {
            if bash_fold_applies(rows) {
                BASH_FOLD_HEAD_ROWS + BASH_FOLD_CHROME_ROWS + BASH_FOLD_TAIL_ROWS
            } else {
                rows
            }
        };
        for rows in 0..200 {
            assert!(
                folded_rows(rows) <= rows,
                "{rows} rows must not grow when folded ({} rendered)",
                folded_rows(rows),
            );
        }
        // And the boundary is exactly one row past the folded footprint.
        assert!(!bash_fold_applies(BASH_FOLD_MIN_VERBATIM_ROWS));
        assert!(bash_fold_applies(BASH_FOLD_MIN_VERBATIM_ROWS + 1));
    }
}
