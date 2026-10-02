//! Backend-owned command vocabulary shared by every frontend.
//!
//! Commands are *harness commands*: control-plane operations owned by the
//! session harness (invoked from a composer as `/name`). The daemon owns the
//! vocabulary; frontends only render it.

use serde::{Deserialize, Serialize};

/// One concrete example for a harness command (invoked as `/name`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub struct CommandExample {
    pub command: String,
    pub description: String,
}

/// A canonical harness command and all metadata needed for completion/help.
///
/// Exactly one prose field exists: [`CommandSpec::summary`]. It doubles as the
/// menu line and the inspector/detail text, so keep it informative on its own
/// (a second long-form field would just drift back into near-duplicate prose).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
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
    #[ts(optional)]
    pub category: Option<String>,
    /// First-token verbs this command accepts (`/schedule list` → `list`),
    /// each with its own one-line introduction. Progressive disclosure: the
    /// parent's list stays lean; subcommand detail appears only after the
    /// user types the space.
    #[serde(default)]
    pub subcommands: Vec<CommandSubcommandSpec>,
}

/// One first-token verb of a harness command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub struct CommandSubcommandSpec {
    pub name: String,
    /// One-line introduction shown when completing `/cmd <cursor>`; keep it
    /// distinct from the sibling lines and from the parent summary.
    pub summary: String,
}

/// A non-command trigger that steers users to a canonical command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub struct CommandAlias {
    pub name: String,
    pub target: String,
}

/// Complete backend command vocabulary for one hosted project/session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
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
}

/// Semantic kind of one backend-produced composer completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
pub enum ComposerCompletionKind {
    Slash,
    SlashAlias,
    Intent,
    PathFile,
    PathDir,
    PathExplicit,
}

pub type InputCompletionKind = ComposerCompletionKind;

/// One completion edit produced by the daemon for the composer.
///
/// Replacement offsets are Unicode-scalar indices, not UTF-8 byte offsets or
/// JavaScript UTF-16 code units. That gives every frontend one stable wire
/// representation; clients translate to their native string indexing only at
/// the final edit boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/generated/wire.gen.ts"))]
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
    #[ts(optional)]
    pub alias_of: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub command: Option<CommandSpec>,
}

pub type InputCompletion = ComposerCompletion;
