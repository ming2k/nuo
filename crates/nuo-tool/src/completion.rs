//! Backend-owned command vocabulary shared by every frontend.
//!
//! A command here is a **slash command**: a `/name` control-plane operation
//! scoped to the current thread (its model, context, tools, and session), owned
//! and executed by the server. The server owns the vocabulary; frontends only
//! render it.
//!
//! The name matters, because the boundary does: a *workspace view* (the
//! dashboard, settings — §[INV-CMD-01], ADR-0041) is a client-navigation surface,
//! not a command, and must never enter this vocabulary. Calling these
//! "harness commands" invited exactly that confusion, so the vocabulary is
//! spelled for its scope instead of for the layer that happens to run it.

use serde::{Deserialize, Serialize};

/// What a slash command does, which is what decides whether it is *remembered*.
///
/// This is the one distinction the durable command ledger exists to make, and it
/// is a property of the command — declared once, next to its summary, in the
/// server's single vocabulary table — never a judgement re-made at a handler.
///
/// - [`CommandEffect::Mutation`] is an **act**: it changes durable state the
///   thread must remember (posture, role, provider selection, trust grants,
///   compaction, thread topology, workspace files, background jobs). Its truth
///   does not decay, so it earns a durable record and a row at its timestamp
///   seam in the rendered thread.
/// - [`CommandEffect::Reading`] is a **report** of the state of the world at the
///   instant it is asked (`/usage`, `/diff`, `/jobs`, `/help`, …). Re-running it
///   produces a different answer by design, so it is stale the moment it exists.
///   It earns no durable record and no thread row; its reply is an ephemeral
///   receipt on the client.
///
/// Both classes still reply on the wire (`nuo_protocol::RoundEvent::CommandResult`
/// carries the class). Ephemeral is not the same as silent: a reading is answered
/// in full, it is simply not asserted later as still true. See ADR-0140.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandEffect {
    /// An act: durable state changed, so the ledger keeps it.
    Mutation,
    /// A report: a measurement of the world, true only at the moment of asking.
    ///
    /// The `Default`, so a vocabulary row or a wire frame that states no effect
    /// makes no claim to durability. Being wrong in this direction costs a
    /// missing row; being wrong in the other direction asserts a stale
    /// measurement as history.
    #[default]
    Reading,
}

impl CommandEffect {
    /// Whether this class earns a durable ledger record.
    pub fn is_durable(self) -> bool {
        matches!(self, Self::Mutation)
    }
}

/// One concrete example for a slash command (invoked as `/name`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandExample {
    pub command: String,
    pub description: String,
}

/// A canonical slash command and all metadata needed for completion/help.
///
/// Exactly one prose field exists: [`CommandSpec::summary`]. It doubles as the
/// menu line and the inspector/detail text, so keep it informative on its own
/// (a second long-form field would just drift back into near-duplicate prose).
///
/// `effect` is the command's declared [`CommandEffect`] (ADR-0140): it tells a
/// frontend, *before dispatch*, whether to stage an optimistic thread row. It is
/// an optimisation only — `RoundEvent::CommandResult` carries the
/// authoritative class after the fact, so a client that never fetched a catalog
/// still files the reply correctly.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSpec {
    pub name: String,
    pub summary: String,
    #[serde(default)]
    pub usage: Vec<String>,
    #[serde(default)]
    pub examples: Vec<CommandExample>,
    #[serde(default)]
    pub intent_keywords: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Whether an invocation of this command is an act (recorded) or a reading
    /// (ephemeral). This is the argument-less invocation's class.
    #[serde(default)]
    pub effect: CommandEffect,
    /// The class of an invocation whose first argument token is not a declared
    /// verb. A handler's `_` arm is what actually runs then, so this states *its*
    /// effect — `/jobs whatever` lists jobs, hence `Reading`, even though `kill`
    /// mutates. Mandatory (no default): a default is exactly how `/role <dynamic
    /// role id>` would be silently misfiled as a reading, and the two errors are
    /// not symmetric — a missing record is unrecoverable, a spurious one is only
    /// noisy (ADR-0140 §Decision).
    #[serde(default)]
    pub verb_effect: CommandEffect,
    /// First-token verbs this command accepts (`/schedule list` → `list`),
    /// each with its own one-line introduction. Progressive disclosure: the
    /// parent's list stays lean; subcommand detail appears only after the
    /// user types the space.
    #[serde(default)]
    pub subcommands: Vec<CommandSubcommandSpec>,
}

/// One first-token verb of a slash command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSubcommandSpec {
    pub name: String,
    /// One-line introduction shown when completing `/cmd <cursor>`; keep it
    /// distinct from the sibling lines and from the parent summary.
    pub summary: String,
    /// The verb's own declared effect. A verb mutates or reads independently of
    /// its parent: `/jobs kill` is an act inside a reading command, `/trust
    /// status` is a reading inside an act.
    #[serde(default)]
    pub effect: CommandEffect,
}

/// A non-command trigger that steers users to a canonical command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSuggestion {
    pub trigger: String,
    pub target: String,
    pub reason: String,
}

/// An accepted compatibility spelling (`/setup`) and its canonical command
/// (`/init`). Aliases are first-class completion candidates: they surface
/// under their own name so the user's mental model ("I typed `set`, I pick
/// `setup`") is preserved in the menu. Selecting the row, however, commits
/// the canonical target into the composer via `insert_text` — the edit is
/// what runs, so the transcript never accumulates legacy spellings. Dispatch
/// still resolves any alias that arrives verbatim (typed, not picked).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandAlias {
    pub name: String,
    pub target: String,
}

/// Complete backend command vocabulary for one hosted project/session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandCatalog {
    pub commands: Vec<CommandSpec>,
    /// Accepted compatibility aliases are dispatchable but not advertised as
    /// completion rows.
    #[serde(default)]
    pub aliases: Vec<CommandAlias>,
    #[serde(default)]
    pub suggestions: Vec<CommandSuggestion>,
}

impl CommandCatalog {
    pub fn find(&self, name: &str) -> Option<&CommandSpec> {
        self.commands
            .iter()
            .find(|command| command.name == name)
            .or_else(|| {
                let target = self
                    .aliases
                    .iter()
                    .find(|alias| alias.name == name)
                    .map(|alias| alias.target.as_str())?;
                self.commands.iter().find(|command| command.name == target)
            })
    }

    pub fn recognizes(&self, name: &str) -> bool {
        self.find(name).is_some()
    }

    pub fn alias(&self, name: &str) -> Option<&CommandAlias> {
        self.aliases.iter().find(|alias| alias.name == name)
    }

    /// The declared effect of `name` (with leading slash, e.g. `/jobs`) and its
    /// first argument token, if any.
    ///
    /// This mirrors the server's `startup::effect_for` exactly, for the one
    /// consumer that runs *before* dispatch (staging an optimistic row). A name
    /// the catalog does not know — an application-registered handler, a project
    /// command prompt — is `Mutation`: extension code may have mutated anything,
    /// and over-recording is the safe direction. A declared verb uses its own
    /// declaration; an undeclared argument uses the command's `verb_effect`, which
    /// is the handler's `_` arm semantics (ADR-0140).
    pub fn effect_for(&self, name: &str, sub: Option<&str>) -> CommandEffect {
        let Some(spec) = self.find(name) else {
            return CommandEffect::Mutation;
        };
        match sub {
            Some(sub) => spec
                .subcommands
                .iter()
                .find(|s| s.name == sub)
                .map_or(spec.verb_effect, |verb| verb.effect),
            None => spec.effect,
        }
    }
}

/// Semantic kind of one backend-produced composer completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComposerCompletionKind {
    Slash,
    SlashAlias,
    Intent,
    PathFile,
    PathDir,
    PathExplicit,
}

pub type InputCompletionKind = ComposerCompletionKind;

/// One completion edit produced by the server for the composer.
///
/// Replacement offsets are Unicode-scalar indices, not UTF-8 byte offsets or
/// JavaScript UTF-16 code units. That gives every frontend one stable wire
/// representation; clients translate to their native string indexing only at
/// the final edit boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComposerCompletion {
    /// Text shown in the completion menu.
    pub label: String,
    pub description: String,
    /// Exact text the client splices over `replace_start..replace_end`.
    /// This is separate from `label` so backend-owned edit behavior (for
    /// example consuming an `@` trigger or appending a trailing space) does
    /// not leak back into frontend code.
    pub insert_text: String,
    pub replace_start: usize,
    pub replace_end: usize,
    pub kind: ComposerCompletionKind,
    /// Set when this row is a compatibility alias: the **canonical** command
    /// the alias resolves to (e.g. `/yolo` → `/delegate`). The label stays
    /// the alias spelling the user typed, but `insert_text` is the canonical
    /// target — accepting the row rewrites the composer to the real command,
    /// so what lands in the transcript is what actually runs. Frontends also
    /// use this field (never description-text sniffing) to render the row
    /// distinctly from canonical commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias_of: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<CommandSpec>,
}

pub type InputCompletion = ComposerCompletion;
