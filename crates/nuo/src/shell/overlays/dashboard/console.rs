use super::*;

pub(crate) fn draw_console(
    frame: &mut Frame,
    area: Rect,
    log: &[ConsoleLine],
    row: Option<&MonitoredThread>,
    focused: bool,
    scroll: &mut usize,
    theme: &Theme,
) -> Rect {
    let (_, body) = inset_panel(frame, area, "Console", focused, theme);
    let lines = console_lines(log, row, body.width as usize, theme);
    render_scrollable(frame, body, lines, scroll, None, theme);
    body
}

/// Render the focused thread's cognitive mission dossier and live telemetry (ADR-0129).

pub(crate) fn draw_console_log(
    frame: &mut Frame,
    area: Rect,
    log: &[ConsoleLine],
    focused: bool,
    scroll: &mut usize,
    theme: &Theme,
) -> Rect {
    let (_, body) = inset_panel(frame, area, "Directives & Receipts", focused, theme);
    let mut lines = Vec::new();
    if log.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Fleet Control: ", Style::default().fg(theme.brand())),
            Span::styled(
                "@3 text sends  /kill /interrupt /suspend /new  bare text prompts selection",
                Style::default().fg(theme.muted()),
            ),
        ]));
    } else {
        for entry in log {
            lines.push(entry.to_line(theme));
        }
    }
    render_scrollable(frame, body, lines, scroll, None, theme);
    body
}

/// Render the decoupled Model Usage & System Analytics view on the dashboard (ADR-0129 [INV-USAGE-01]).

// the console's command grammar

/// One line of the console transcript: what was dispatched and how the
/// server answered. Kept as typed data (not preformatted strings) so the
/// renderer owns all styling and the tests can assert on structure.
#[derive(Debug, Clone, PartialEq)]
pub enum ConsoleLine {
    /// A directive the user issued: the raw text plus the resolved targets
    /// (`#3`, `#2 #3`, or `new`) and the action taken.
    Dispatch {
        raw: String,
        targets: Vec<usize>,
        action: &'static str,
    },
    /// The server's answer to a dispatch: `ok` receipts in `theme.ok()`,
    /// failures in `theme.err()` with the server's error text.
    Receipt {
        ok: bool,
        target: Option<usize>,
        text: String,
    },
    /// A local notice (parse error, unknown session, confirmation hint).
    Notice(String),
}

impl ConsoleLine {
    /// Render one console line. `theme` owns the palette; widths are
    /// unconstrained (the console wraps nothing — receipts are one line).
    fn to_line(&self, theme: &Theme) -> Line<'static> {
        match self {
            Self::Dispatch {
                raw,
                targets,
                action,
            } => {
                let who = if targets.is_empty() {
                    "new".to_string()
                } else {
                    targets
                        .iter()
                        .map(|n| format!("#{n}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                };
                Line::from(vec![
                    Span::styled(INPUT_PROMPT_PREFIX, Style::default().fg(theme.brand())),
                    Span::styled(format!("[{who}] "), Style::default().fg(theme.brand())),
                    Span::styled(action.to_string(), Style::default().fg(theme.muted())),
                    Span::styled(format!("  {raw}"), Style::default().fg(theme.fg())),
                ])
            }
            Self::Receipt { ok, target, text } => {
                let (glyph, color) = if *ok {
                    ("✓", theme.ok())
                } else {
                    ("✗", theme.err())
                };
                let target_span = target
                    .map(|n| Span::styled(format!("#{n} "), Style::default().fg(theme.brand())))
                    .unwrap_or_else(|| Span::raw(String::new()));
                Line::from(vec![
                    Span::styled("  ".to_string(), Style::default()),
                    Span::styled(format!("{glyph} "), Style::default().fg(color)),
                    target_span,
                    Span::styled(text.clone(), Style::default().fg(color)),
                ])
            }
            Self::Notice(text) => Line::from(vec![
                Span::styled("  ".to_string(), Style::default()),
                Span::styled(text.clone(), Style::default().fg(theme.dim())),
            ]),
        }
    }
}

/// A parsed console directive (grammar plus slash verbs).
#[derive(Debug, Clone, PartialEq)]
pub enum ConsoleCommand {
    /// `@3 text` / `@2 @3 text` — send `text` as a new round to each `#N`.
    Prompt { targets: Vec<usize>, text: String },
    /// `/kill [@N]`, `/interrupt [@N]`, `/suspend [@N]` — a control verb on
    /// one session. No `@N` means the dock selection.
    Verb {
        verb: ConsoleVerb,
        target: Option<usize>,
    },
    /// `/new text` — create a session for the dashboard's project with
    /// `text` as the opening prompt.
    New { text: Option<String> },
    /// `? text` / `/ask text` — ask the Archivist, the
    /// nuo-level retrieval agent, one question. The answer arrives as a
    /// console receipt (synchronous control round).
    Archivist { text: String },
    /// `/help` — the verb table as a notice block.
    Help,
    /// Text that matched no rule (empty input, a bare `/` with no verb, …).
    Unrecognized(String),
}

/// The verbs the console dispatches to the control plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleVerb {
    Kill,
    Interrupt,
    Suspend,
}

impl ConsoleVerb {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Kill => "kill",
            Self::Interrupt => "interrupt",
            Self::Suspend => "suspend",
        }
    }

    /// One-line summary for the `/help` notice block.
    pub fn help_line(self) -> &'static str {
        match self {
            Self::Kill => "/kill [@N]     tear the session down (history kept on disk)",
            Self::Interrupt => "/interrupt [@N] stop the session's current round",
            Self::Suspend => "/suspend [@N]  park it in memory; next attach resumes it",
        }
    }
}

/// Parse one console line. The grammar (plus verbs):
///
/// - `@3 refactor the retry loop` → [`ConsoleCommand::Prompt`] targeting `#3`
/// - `@2 @3 summarize` → fan-out to `#2` and `#3`
/// - `/kill`, `/kill @3` → [`ConsoleCommand::Verb`]
/// - `/new <text>`, `/new` → [`ConsoleCommand::New`]
/// - `? <text>` / `/ask <text>` → [`ConsoleCommand::Archivist`]
/// - `/help` → [`ConsoleCommand::Help`]
/// - anything else that is non-empty → [`ConsoleCommand::Prompt`] with no
///   targets (the caller resolves "no targets" to the dock selection)
pub fn parse_console_command(line: &str) -> ConsoleCommand {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return ConsoleCommand::Unrecognized(String::new());
    }

    // The Archivist address: `? text` — a single leading `?` distinguishes it
    // from `@N` (session addresses) and `/` (verbs). No address follows it.
    if let Some(rest) = trimmed.strip_prefix('?') {
        let text = rest.trim();
        if text.is_empty() {
            return ConsoleCommand::Unrecognized(
                "nothing to ask — the Archivist needs a question after '?'".to_string(),
            );
        }
        return ConsoleCommand::Archivist {
            text: text.to_string(),
        };
    }

    // Slash verbs first: `/verb [rest…]`.
    if let Some(rest) = trimmed.strip_prefix('/') {
        let mut parts = rest.splitn(2, char::is_whitespace);
        let word = parts.next().unwrap_or("").to_ascii_lowercase();
        let remainder = parts.next().unwrap_or("").trim().to_string();
        return match word.as_str() {
            "help" | "?" => ConsoleCommand::Help,
            "ask" => {
                if remainder.is_empty() {
                    ConsoleCommand::Unrecognized(
                        "nothing to ask — the Archivist needs a question after /ask".to_string(),
                    )
                } else {
                    ConsoleCommand::Archivist { text: remainder }
                }
            }
            "new" => {
                if remainder.is_empty() {
                    ConsoleCommand::New { text: None }
                } else {
                    ConsoleCommand::New {
                        text: Some(remainder),
                    }
                }
            }
            "kill" | "x" => parse_verb(ConsoleVerb::Kill, &remainder),
            "interrupt" | "stop" => parse_verb(ConsoleVerb::Interrupt, &remainder),
            "suspend" | "park" => parse_verb(ConsoleVerb::Suspend, &remainder),
            _ => ConsoleCommand::Unrecognized(format!(
                "unknown command '{word}' — /help lists the verbs"
            )),
        };
    }

    // Then the address grammar: a leading run of `@n` tokens.
    let (targets, text) = split_leading_targets(trimmed);
    ConsoleCommand::Prompt {
        targets,
        text: text.to_string(),
    }
}

/// `/verb [@N]` — an optional single address token after the verb word.
fn parse_verb(verb: ConsoleVerb, rest: &str) -> ConsoleCommand {
    let rest = rest.trim();
    if rest.is_empty() {
        return ConsoleCommand::Verb { verb, target: None };
    }
    let (targets, leftover) = split_leading_targets(rest);
    match (targets.first().copied(), leftover.trim().is_empty()) {
        (Some(n), true) => ConsoleCommand::Verb {
            verb,
            target: Some(n),
        },
        (None, true) => ConsoleCommand::Verb { verb, target: None },
        // Anything beyond the verb (and an optional address) is a usage
        // error; report it instead of silently discarding the tail.
        _ => ConsoleCommand::Unrecognized(format!(
            "usage: /{} [@N] — nothing else follows the verb",
            verb.as_str()
        )),
    }
}

/// Split a leading run of `@n` address tokens from the payload: returns
/// `(targets, rest)`. An `@` token that fails to parse as a number is left
/// in the payload (it is prose, not an address).
fn split_leading_targets(text: &str) -> (Vec<usize>, &str) {
    let mut targets = Vec::new();
    let mut rest = text;
    while let Some(after) = rest.strip_prefix('@') {
        // Digits immediately after the `@` form the number.
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            break;
        }
        match digits.parse::<usize>() {
            Ok(n) => {
                targets.push(n);
                // Skip exactly one whitespace run between tokens.
                rest = after[digits.len()..].trim_start();
            }
            Err(_) => break,
        }
    }
    (targets, rest)
}

/// The console transcript rendering: receipts above the fold, the hint
/// header, then the selected session's monitor read-out.
pub(crate) fn console_lines(
    log: &[ConsoleLine],
    row: Option<&MonitoredThread>,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            "Direct the fleet: ".to_string(),
            Style::default().fg(theme.muted()),
        ),
        Span::styled("@3 text".to_string(), Style::default().fg(theme.brand())),
        Span::styled(" sends  ".to_string(), Style::default().fg(theme.dim())),
        Span::styled(
            "/kill /interrupt /suspend /new /help".to_string(),
            Style::default().fg(theme.brand()),
        ),
        Span::styled(
            " manage  bare text prompts the selection".to_string(),
            Style::default().fg(theme.dim()),
        ),
    ]));
    if log.is_empty() {
        lines.push(Line::from(Span::styled(
            "No directives yet.".to_string(),
            Style::default().fg(theme.dim()),
        )));
    } else {
        lines.extend(log.iter().map(|l| l.to_line(theme)));
    }
    if let Some(row) = row {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "selected session".to_string(),
            Style::default().fg(theme.dim()),
        )));
        lines.extend(session_detail_lines(row, width, theme));
    }
    lines
}
