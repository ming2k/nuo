use nuo_wire::tool_output::{
    ShellLine, ShellStream, normalize_carriage_returns, strip_ansi, truncate_utf8,
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{ChildStderr, ChildStdout};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::task::JoinHandle;

pub const SHELL_COLLECT_MAX_CHARS: usize = nuo_wire::tool_output::SHELL_MAX_OUTPUT_CHARS * 8;
pub const SHELL_COLLECT_MAX_LINES: usize = 5_000;

/// Maximum lines a foreground synchronous command may produce before StreamGuard terminates it early (ADR-0257).
pub const SHELL_STREAM_FLOOD_LINES: usize = 1_000;
/// Maximum bytes a foreground synchronous command may produce before StreamGuard terminates it early (ADR-0257).
pub const SHELL_STREAM_FLOOD_BYTES: usize = 128 * 1024;
/// Maximum length of a single unwrapped line before being classified as a minified artifact (ADR-0264).
pub const MAX_UNWRAPPED_LINE_LEN: usize = 4_096;
/// Maximum consecutive periodic samples before StreamGuard declares instantaneous snapshot sufficiency (ADR-0257).
pub const STREAM_METRONOMIC_SAMPLE_LIMIT: usize = 4;
/// Maximum full screen redraws before StreamGuard declares TUI snapshot sufficiency (ADR-0257).
pub const TUI_REDRAW_LIMIT: usize = 2;

/// Tracks physical stream arrival cadence and structural entropy to identify
/// metronomic polling monitors (intel_gpu_top, vmstat, ping) and TUI screen redraws early.
#[derive(Debug, Default)]
pub struct StreamCadenceTracker {
    last_line_instant: Option<std::time::Instant>,
    periodic_streak: usize,
    last_token_count: Option<usize>,
    tui_redraw_count: usize,
}

impl StreamCadenceTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Observe a newly arrived output line with current timestamp.
    pub fn observe(&mut self, text: &str) -> bool {
        self.observe_at(text, std::time::Instant::now())
    }

    /// Observe a newly arrived output line at a specific instant (supports deterministic unit tests).
    pub fn observe_at(&mut self, text: &str, now: std::time::Instant) -> bool {
        // 1. TUI screen redraw detection: ANSI cursor-home / screen-clear codes.
        if text.contains("\x1b[H") || text.contains("\x1b[2J") || text.contains("\x1b[1;1H") {
            self.tui_redraw_count += 1;
            if self.tui_redraw_count >= TUI_REDRAW_LIMIT {
                return true;
            }
        }

        // 2. Metronomic periodic cadence detection (e.g. intel_gpu_top -l, vmstat, ping).
        if let Some(prev) = self.last_line_instant {
            let delta = now.saturating_duration_since(prev);
            // Polling interval between 200ms and 3000ms is typical of CLI status monitors.
            if delta >= std::time::Duration::from_millis(200)
                && delta <= std::time::Duration::from_millis(3000)
            {
                let token_count = text.split_whitespace().count();
                // If consecutive lines share structural token density (numeric data rows),
                // it is an active polling stream.
                if let Some(last_count) = self.last_token_count
                    && last_count > 0
                    && last_count == token_count
                {
                    self.periodic_streak += 1;
                    if self.periodic_streak >= STREAM_METRONOMIC_SAMPLE_LIMIT {
                        return true;
                    }
                } else {
                    self.last_token_count = Some(token_count);
                }
            } else if delta < std::time::Duration::from_millis(100) {
                // High-speed bursts (e.g. compilation batches) reset the periodic cadence streak.
                self.periodic_streak = 0;
            }
        }
        self.last_line_instant = Some(now);
        false
    }
}

/// Background reader tasks draining child stdout and stderr concurrently into a merged channel.
pub struct StreamReaders {
    pub rx: UnboundedReceiver<(ShellStream, String)>,
    pub stdout_task: JoinHandle<()>,
    pub stderr_task: JoinHandle<()>,
}

/// Spawn the command with piped stdout/stderr and merge both streams into a
/// single, arrival-ordered line buffer so the renderer never has to choose
/// the "all-stdout-then-all-stderr" split.
pub fn spawn_stream_readers(stdout: ChildStdout, stderr: ChildStderr) -> StreamReaders {
    let (tx, rx) = unbounded_channel::<(ShellStream, String)>();

    let tx_err = tx.clone();
    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = tx_err.send((
                ShellStream::Err,
                normalize_carriage_returns(&strip_ansi(&line)),
            ));
        }
    });

    let tx_out = tx.clone();
    let stdout_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = tx_out.send((
                ShellStream::Out,
                normalize_carriage_returns(&strip_ansi(&line)),
            ));
        }
    });
    drop(tx);

    StreamReaders {
        rx,
        stdout_task,
        stderr_task,
    }
}

/// In-memory collection buffer for lines, stdout, and stderr.
///
/// Enforces online bounded memory caps during command execution so runaway
/// outputs (e.g. `find /`, `yes`, or massive build logs) never inflate
/// memory or crash the process before process termination.
///
/// Also coalesces high-frequency line output into streaming batches to prevent
/// UI event loops from being flooded and dropping frames.
pub struct OutputCollector {
    pub stdout_buf: String,
    pub stderr_buf: String,
    pub lines: Vec<ShellLine>,
    pub raw_bytes: Vec<u8>,
    truncated: bool,
    pending_stdout: String,
    pending_stderr: String,
    last_flush: std::time::Instant,
    cadence_tracker: StreamCadenceTracker,
    cadence_flooded: bool,
}

impl Default for OutputCollector {
    fn default() -> Self {
        Self {
            stdout_buf: String::new(),
            stderr_buf: String::new(),
            lines: Vec::new(),
            raw_bytes: Vec::new(),
            truncated: false,
            pending_stdout: String::new(),
            pending_stderr: String::new(),
            last_flush: std::time::Instant::now(),
            cadence_tracker: StreamCadenceTracker::new(),
            cadence_flooded: false,
        }
    }
}

impl OutputCollector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_line(
        &mut self,
        stream: ShellStream,
        text: String,
        on_stream: &mut (dyn FnMut(nuo_wire::ToolStream) + Send + '_),
    ) {
        // ADR-0276: capture byte-exact raw stream before minification, ANSI stripping, or folding.
        self.raw_bytes.extend_from_slice(text.as_bytes());
        self.raw_bytes.push(b'\n');

        if self.cadence_tracker.observe(&text) {
            self.cadence_flooded = true;
        }

        // ADR-0264: Content-Aware Ingestion Gate for unwrapped minified lines.
        // Prevent single massive minified lines (e.g. 100KB+ JS bundles / CSS modules)
        // from flooding inline buffers while preserving structural information.
        let is_minified = text.len() > MAX_UNWRAPPED_LINE_LEN;
        let displayed_text = if is_minified {
            self.truncated = true;
            let head = truncate_utf8(&text, 256);
            format!(
                "{head} ... [minified line: {} bytes omitted to protect context budget]",
                text.len().saturating_sub(256)
            )
        } else {
            text.clone()
        };

        match stream {
            ShellStream::Out => {
                self.stdout_buf.push_str(&displayed_text);
                self.stdout_buf.push('\n');
                self.pending_stdout.push_str(&displayed_text);
                self.pending_stdout.push('\n');
            }
            ShellStream::Err => {
                self.stderr_buf.push_str(&displayed_text);
                self.stderr_buf.push('\n');
                self.pending_stderr.push_str(&displayed_text);
                self.pending_stderr.push('\n');
            }
        }
        self.lines.push(ShellLine {
            stream,
            text: displayed_text,
        });

        self.compact_in_flight_if_needed();

        if self.last_flush.elapsed() >= std::time::Duration::from_millis(30)
            || self.pending_stdout.len() >= 4096
            || self.pending_stderr.len() >= 4096
        {
            self.flush_stream(on_stream);
        }
    }

    /// Flush any buffered streaming lines out to the UI.
    pub fn flush_stream(
        &mut self,
        on_stream: &mut (dyn FnMut(nuo_wire::ToolStream) + Send + '_),
    ) {
        if !self.pending_stdout.is_empty() {
            on_stream(nuo_wire::ToolStream::Stdout(std::mem::take(
                &mut self.pending_stdout,
            )));
        }
        if !self.pending_stderr.is_empty() {
            on_stream(nuo_wire::ToolStream::Stderr(std::mem::take(
                &mut self.pending_stderr,
            )));
        }
        self.last_flush = std::time::Instant::now();
    }

    fn compact_in_flight_if_needed(&mut self) {
        if self.stdout_buf.len() > SHELL_COLLECT_MAX_CHARS * 2 {
            self.stdout_buf = head_tail(&self.stdout_buf, SHELL_COLLECT_MAX_CHARS / 2);
            self.truncated = true;
        }
        if self.stderr_buf.len() > SHELL_COLLECT_MAX_CHARS * 2 {
            self.stderr_buf = head_tail(&self.stderr_buf, SHELL_COLLECT_MAX_CHARS / 2);
            self.truncated = true;
        }
        if self.lines.len() > SHELL_COLLECT_MAX_LINES * 2 {
            let half = SHELL_COLLECT_MAX_LINES / 2;
            let dropped = self.lines.len() - (half * 2);
            let marker = ShellLine {
                stream: ShellStream::Err,
                text: format!("⋯ {dropped} lines dropped (in-flight collection cap)"),
            };
            let mut capped: Vec<ShellLine> = self.lines.drain(..half).collect();
            capped.push(marker);
            capped.extend(self.lines.drain(self.lines.len() - half..));
            self.lines = capped;
            self.truncated = true;
        }
    }

    pub fn drain_remaining_rx(&mut self, rx: &mut UnboundedReceiver<(ShellStream, String)>) {
        while let Ok((stream, text)) = rx.try_recv() {
            // ADR-0276: capture byte-exact raw stream before minification, ANSI stripping, or folding.
            self.raw_bytes.extend_from_slice(text.as_bytes());
            self.raw_bytes.push(b'\n');

            match stream {
                ShellStream::Out => {
                    self.stdout_buf.push_str(&text);
                    self.stdout_buf.push('\n');
                }
                ShellStream::Err => {
                    self.stderr_buf.push_str(&text);
                    self.stderr_buf.push('\n');
                }
            }
            self.lines.push(ShellLine { stream, text });
        }
        self.compact_in_flight_if_needed();
    }

    /// The un-truncated, un-folded raw bytes captured across the execution (ADR-0276).
    #[allow(dead_code)]
    pub fn raw_bytes(&self) -> &[u8] {
        &self.raw_bytes
    }

    /// True when at least one output line was captured. A child that has
    /// produced output and then gone silent is the ADR-0190 detach signature
    /// (service banner, then listen-loop quiet); silence-from-birth is not.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Check whether a foreground synchronous command has exceeded continuous streaming flood limits (ADR-0257).
    ///
    /// When `raw` is false, commands that continuously emit streaming output without self-terminating
    /// (e.g. `intel_gpu_top -l`, `top`, `ping`, `tail -f`) are caught early once they reach the flood threshold,
    /// preventing turn hangs and context-window blowup.
    /// In `raw: true` mode, the threshold relaxes to `SHELL_COLLECT_MAX_LINES` / `SHELL_COLLECT_MAX_CHARS`
    /// to support intentional deep log inspection.
    pub fn is_stream_flooded(&self, raw: bool) -> bool {
        if raw {
            self.lines.len() >= SHELL_COLLECT_MAX_LINES
                || (self.stdout_buf.len() + self.stderr_buf.len()) >= SHELL_COLLECT_MAX_CHARS
        } else {
            self.cadence_flooded
                || self.lines.len() >= SHELL_STREAM_FLOOD_LINES
                || (self.stdout_buf.len() + self.stderr_buf.len()) >= SHELL_STREAM_FLOOD_BYTES
        }
    }

    /// The captured lines in arrival order (for adoption replay).
    #[allow(dead_code)]
    pub fn lines(&self) -> &[ShellLine] {
        &self.lines
    }

    /// Apply head+tail byte caps and line count caps to prevent unbound memory growth.
    #[allow(dead_code)]
    pub fn apply_caps(self, exit: Option<i32>) -> (String, String, Vec<ShellLine>, bool) {
        self.apply_caps_ex(exit, false)
    }

    /// Apply caps with optional bypass of semantic folding when `raw` is true (ADR-0254).
    pub fn apply_caps_ex(
        mut self,
        exit: Option<i32>,
        raw: bool,
    ) -> (String, String, Vec<ShellLine>, bool) {
        // ADR-0254 Ingestion-time semantic folding:
        // When not in raw mode, deterministically collapse consecutive pure-green passing test
        // runs (Ninja/Meson/Nextest) while preserving 100% of failures, warnings, and stderr.
        if !raw {
            let (folded_lines, folded_count) =
                fold_pure_green_runs(std::mem::take(&mut self.lines));
            if folded_count > 0 {
                let mut new_stdout = String::new();
                for line in &folded_lines {
                    if line.stream == ShellStream::Out {
                        new_stdout.push_str(&line.text);
                        new_stdout.push('\n');
                    }
                }
                new_stdout.push_str(&format!(
                    "\n[Note: {folded_count} pure-green test pass lines folded. Use `raw: true` if you need unabridged test output.]\n"
                ));
                self.stdout_buf = new_stdout;
            }
            self.lines = folded_lines;
        }

        let mut collection_truncated = self.truncated;
        if self.stdout_buf.len() > SHELL_COLLECT_MAX_CHARS {
            self.stdout_buf = head_tail(&self.stdout_buf, SHELL_COLLECT_MAX_CHARS / 2);
            collection_truncated = true;
        }
        if self.stderr_buf.len() > SHELL_COLLECT_MAX_CHARS {
            self.stderr_buf = head_tail(&self.stderr_buf, SHELL_COLLECT_MAX_CHARS / 2);
            collection_truncated = true;
        }
        if self.lines.len() > SHELL_COLLECT_MAX_LINES {
            let half = SHELL_COLLECT_MAX_LINES / 2;
            let dropped = self.lines.len() - (half * 2);
            let marker = ShellLine {
                stream: ShellStream::Err,
                text: format!("⋯ {dropped} lines dropped (collection cap)"),
            };
            let mut capped: Vec<ShellLine> = self.lines.drain(..half).collect();
            capped.push(marker);
            capped.extend(self.lines.drain(self.lines.len() - half..));
            self.lines = capped;
            collection_truncated = true;
        }

        let truncated = collection_truncated
            || nuo_wire::tool_output::shell_inner_text(
                &self.stdout_buf,
                &self.stderr_buf,
                exit,
            )
            .len()
                > nuo_wire::tool_output::SHELL_MAX_OUTPUT_CHARS;

        (self.stdout_buf, self.stderr_buf, self.lines, truncated)
    }
}

/// Check if a stdout line represents a pure-green test pass line (Ninja, Meson, Nextest, Jest, TAP, etc.)
pub fn is_pure_green_test_line(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    let lower = t.to_ascii_lowercase();
    // Invariant: any line containing warnings, errors, failures, leaks, or panics is NEVER folded.
    if lower.contains("warn")
        || lower.contains("fail")
        || lower.contains("panic")
        || lower.contains("error")
        || lower.contains("leak")
        || lower.contains("assert")
    {
        return false;
    }

    // Pattern 1: Ninja / Meson progress and pass lines:
    // e.g. `[1/134] test_foo OK 0.01s`, `[ 2/134] test_bar OK 0.02s`, or `1/134 test_foo OK 0.01s`
    if (t.starts_with('[')
        && t.contains('/')
        && (t.contains(" OK") || t.ends_with(" OK") || t.contains(" PASSED")))
        || (t.contains('/')
            && (t.ends_with(" OK")
                || t.ends_with(" PASSED")
                || t.contains(" OK ")
                || t.contains(" PASSED ")))
    {
        return true;
    }

    // Pattern 2: Cargo / Nextest:
    // e.g. `PASS [   0.004s] crate::test_name` or `test crate::test_name ... ok`
    if t.starts_with("PASS [") || t.ends_with("... ok") || t.ends_with("... OK") {
        return true;
    }

    // Pattern 3: Jest / TAP / Generic checkmark pass:
    // e.g. `✓ test_name` or `ok 1 - test_name`
    if t.starts_with("✓ ") || (t.starts_with("ok ") && t.contains(" - ")) {
        return true;
    }

    false
}

/// Deterministically fold runs of pure-green passing test lines (threshold >= 3 consecutive lines).
pub fn fold_pure_green_runs(lines: Vec<ShellLine>) -> (Vec<ShellLine>, usize) {
    let mut out = Vec::with_capacity(lines.len());
    let mut i = 0;
    let mut total_folded = 0;

    while i < lines.len() {
        if lines[i].stream == ShellStream::Out && is_pure_green_test_line(&lines[i].text) {
            let mut j = i;
            while j < lines.len()
                && lines[j].stream == ShellStream::Out
                && is_pure_green_test_line(&lines[j].text)
            {
                j += 1;
            }
            let count = j - i;
            if count >= 3 {
                out.push(ShellLine {
                    stream: ShellStream::Out,
                    text: format!("⋯ {count} tests passed (pure-green output folded)"),
                });
                total_folded += count - 1;
            } else {
                for line in &lines[i..j] {
                    out.push(line.clone());
                }
            }
            i = j;
        } else {
            out.push(lines[i].clone());
            i += 1;
        }
    }

    (out, total_folded)
}

/// Keep the first `head` and last `head` bytes of `s` (UTF-8-safe, without
/// splitting a character), joining them with a marker row.
pub fn head_tail(s: &str, head: usize) -> String {
    if s.len() <= head * 2 {
        return s.to_string();
    }
    let total = s.len();
    format!(
        "{}\n⋯ {} bytes dropped (collection cap)\n{}",
        truncate_utf8(s, head),
        total - head * 2,
        truncate_utf8(&s[total - head..], head)
    )
}
