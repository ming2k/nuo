//! Tool invocation I/O vocabulary: streaming chunks, input-execution contracts,
//! and shell termination/line types (ADR-0008 `[INV-TOOL-11]`).
//!
//! These are pure data types with no dependency on any higher layer, so they
//! live in the tool leaf where every capability crate can reach them. A tool
//! streams progress through the [`ToolStream`] sink in its `ToolContext`; the
//! input-execution contract ([`InputContract`]) tells the command tool how a
//! child's stdin/terminal is provisioned; and [`ShellTermination`] /
//! [`ShellLine`] carry *why* and *how* a shell step ended.

use serde::{Deserialize, Serialize};

/// An incremental chunk streamed by a long-running tool before its final
/// output lands. Lets the UI render partial output (e.g. a bash command's
/// stdout as it arrives) instead of freezing on a spinner until the process
/// exits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
// The Web app has always called this `ToolStreamFrame` (`ToolStream` is the
// event variant that carries it); keep the established TS name.
pub enum ToolStream {
    /// Bytes appended to the running stdout buffer.
    Stdout(String),
    /// Bytes appended to the running stderr buffer.
    Stderr(String),
}

/// How a child process's input channels are provisioned for one invocation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputContract {
    /// Immediate-EOF stdin (`/dev/null`), no controlling terminal, no runtime
    /// supervision. The default hard floor.
    #[default]
    Sealed,
    /// Harness-held stdin pipe prefilled with `data` (a declared source), then
    /// closed so the child reads the bytes followed by EOF. No terminal.
    Prefilled { data: String },
    /// Held-open stdin pipe, a child-owned controlling terminal (`/dev/tty`),
    /// and runtime examination. `expectation` is the pre-spawn classifier's
    /// advisory hint, used to seed the first prompt and to mask secrets; the
    /// examiner is the source of truth for *whether* input is awaited.
    Supervised {
        expectation: Option<InputExpectation>,
    },
}

/// The pre-spawn classifier's advisory guess about what an interactive command
/// will ask for. Advisory only: correctness rests on the runtime examiner,
/// which reports the command's *actual* wait state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputExpectation {
    /// Human-facing instruction shown in the operator input panel.
    pub prompt: String,
    /// Mask the operator's typing (passwords / passphrases).
    pub secret: bool,
}

/// A runtime request for one line of operator input for a supervised command.
/// Runtime-only (never crosses the wire as such); the agent layer translates
/// it into a stdin-request event.
///
/// The channel the answer goes to is not named here: in the supervised model
/// the child's stdin *is* its controlling terminal, so there is exactly one
/// channel and the platform seam owns it (ADR-0293).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputPrompt {
    /// The command awaiting input, shown for context.
    pub command: String,
    /// Human-facing instruction (the classifier's expectation, or a description
    /// of the detected wait state).
    pub prompt: String,
    /// Mask the operator's typing.
    pub secret: bool,
}

/// Why a shell step stopped. Drives the themed termination footer (L6) so the
/// user and the model can tell *why* a command ended — not just that it did.
/// A healthy `Exited` run is silent; every other variant renders a coloured
/// marker. Back-compat: restored sessions without this field deserialize as
/// [`ShellTermination::Exited`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShellTermination {
    /// The child exited on its own (with whatever `exit` code). The normal
    /// case; the footer reads only `exit N` when non-zero.
    #[default]
    Exited,
    /// No output for longer than the idle budget — the child was almost
    /// certainly blocked waiting for stdin (a prompt the agent cannot answer).
    /// Rendered as a `warn()`-coloured footer with a non-interactive remedy
    /// hint. The child was killed.
    IdleBlocked,
    /// The interactive classifier matched the command (sudo/gpg/passwd/…) and
    /// the operator declined to supply input (or none was reachable). The
    /// command was *not* executed. Rendered as a `warn()`-coloured footer
    /// with the suggested non-interactive flags.
    InteractiveBlocked,
    /// A supervised command reached an input wait, the operator was asked, and
    /// no answer was supplied (declined, or no reachable channel); the child
    /// was killed. Distinct from [`InteractiveBlocked`](Self::InteractiveBlocked)
    /// (refused *before* spawn, by the classifier) — this is a wait the
    /// examiner detected *at runtime*. Rendered with a remedy hint.
    InputUnanswered,
    /// The sync budget expired while the child was still alive and producing
    /// output (ADR-0190 detach-on-budget): the process was *not* killed — it
    /// was adopted by the background-job fabric, results arrive via job
    /// notification. `exit` is `None`; the job id rides the output payload.
    Detached,
    /// The wall-clock timeout ceiling was reached (the command was producing
    /// output but running too long). The child was killed.
    Timeout,
    /// The tool execution was cancelled by an operator interrupt. The child
    /// was killed.
    Cancelled,
    /// The foreground command produced unbounded continuous streaming output
    /// reaching the StreamGuard budget without self-terminating (ADR-0257).
    /// The process was killed and an instantaneous snapshot was preserved.
    StreamGuard,
}

/// Which pipe a captured shell line came from. Lets the renderer colour
/// stderr distinctly while still emitting lines in their true arrival order
/// (interleaved), instead of the all-stdout-then-all-stderr split that lost
/// timing for tools like `cargo`/`git`/`npm`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShellStream {
    /// Standard output.
    Out,
    /// Standard error.
    Err,
}

/// One captured line of shell output with its source stream tagged. The TUI
/// renders a shell output's `lines` verbatim in order (the source tag only
/// picks the colour), which preserves stdout/stderr interleaving. The
/// model-facing text path keeps using the flat `stdout`/`stderr` fields, so the
/// two audiences stay decoupled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellLine {
    pub stream: ShellStream,
    pub text: String,
}

/// Runtime input supervisor for a supervised command invocation. Implemented
/// by the agent layer (which owns the human-input channel) and handed to the
/// command tool through the tool context. Kept as a trait so the tool leaf
/// stays free of agent/async-runtime coupling.
#[async_trait::async_trait]
pub trait InputHandler: Send + Sync {
    /// Resolve one runtime input prompt. `Ok(Some(line))` — the operator's
    /// answer, written into the reported channel. `Ok(None)` — no answer
    /// (declined, or no reachable human channel); the caller kills the child
    /// with [`ShellTermination::InputUnanswered`].
    async fn resolve(&self, prompt: InputPrompt) -> Option<String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_contract_defaults_to_sealed() {
        assert_eq!(InputContract::default(), InputContract::Sealed);
    }

    #[test]
    fn shell_termination_defaults_to_exited() {
        assert_eq!(ShellTermination::default(), ShellTermination::Exited);
    }

    #[test]
    fn tool_stream_round_trips() {
        let s = ToolStream::Stdout("hello".into());
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<ToolStream>(&json).unwrap(), s);
    }
}
