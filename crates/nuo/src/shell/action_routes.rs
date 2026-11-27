//! The composer's slash routing table: one row per client-reachable action
//! ([ADR-0252](../../../docs/adr/0252-one-action-manifest-declared-primitives-and-compositions.md) step 4).
//!
//! # Why this exists
//!
//! `resolve_enter` used to hold a hand-written `match` over string literals deciding what a
//! `/`-prefixed draft meant. That table was the client's **second copy** of a vocabulary
//! the server already owns — the shape ADR-0119 §2b forbids, and the one that had already
//! drifted three ways before anyone measured it.
//!
//! It could not simply be deleted, because a draft's meaning genuinely has three sources:
//!
//! | case | example | owner |
//! | :--- | :--- | :--- |
//! | an action the manifest declares, presented by the client | `/new`, `/exit` | the manifest |
//! | a workspace view the server must not know | `/dashboard`, `/close` | the client |
//! | an action the server dispatches with no local surface | `/compact`, `/undo`, `/fork` | the wire |
//!
//! So the table is kept, but every row states **which** of the three it is. A row that
//! claims a manifest action is checked against that declaration; a client-only row carries
//! its reason inline, so "the server has never heard of this spelling" is a visible
//! decision rather than an absence. That is what turns a second copy of the vocabulary into
//! a projection of it plus a stated allowance (INV-VERB-03, with ADR-0041
//! `[INV-CMD-01]` as the allowance).
//!
//! # Three rules, three tests
//!
//! 1. no spelling appears twice — a duplicate is a routing ambiguity, not a preference;
//! 2. a row claiming [`RouteSource::Manifest`] names a composition whose `slash` is that
//!    very spelling;
//! 3. a declared composition carrying a `slash` has a row, so a reachable action cannot
//!    exist without a way to reach it.

use crate::shell::input::InputAction;
use nuo_protocol::action::ActionId;

/// Where a route's meaning comes from.
#[derive(Debug, Clone, Copy)]
pub enum RouteSource {
    /// Declared in the server's manifest; this row is a projection of that declaration.
    Manifest(ActionId),
    /// The client owns the action outright. ADR-0041 `[INV-CMD-01]` keeps workspace views
    /// out of the thread vocabulary, so a spelling for one may exist here and nowhere else
    /// — but only with its reason stated.
    ClientOnly {
        /// Why this is client navigation rather than a thread action.
        reason: &'static str,
    },
}

/// What a route produces.
///
/// One variant: a row exists only because the client does something with the draft. The
/// wire is not a target a row can name — a spelling the table does not claim is forwarded
/// verbatim, so "send it to the server" needs no row and therefore cannot become a second
/// vocabulary ([`INV-PROV-07`](ADR-0179): wired or deleted).
#[derive(Debug, Clone, Copy)]
pub enum RouteTarget {
    /// A constant client action; the draft's arguments are consumed by it.
    Fixed(fn(String) -> InputAction),
}

/// Whether a route accepts arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgsPolicy {
    /// Matches with or without arguments (`/models gpt-4o`).
    Any,
    /// Matches only as the whole draft. Used where an argument changes the question being
    /// asked: `/context --json` is the machine form, rendered by the server, so a client
    /// that swallowed the argument would answer a different question (ADR-0220 §7).
    ExactOnly,
}

/// One row of the routing table.
#[derive(Debug, Clone, Copy)]
pub struct Route {
    /// Accepted spellings, aliases included, canonical first. Order is documentation, and
    /// an alias cannot survive in a second list because there is no second list.
    pub spellings: &'static [&'static str],
    pub target: RouteTarget,
    pub args: ArgsPolicy,
    pub source: RouteSource,
}

impl Route {
    /// The canonical spelling (the first one).
    pub fn canonical(&self) -> &'static str {
        // `ROUTES` is a literal table, so every row has at least one spelling; the gate
        // `every_route_declares_a_spelling` fails the build's tests if that stops being
        // true, which keeps this index honest without a panic path in production.
        self.spellings.first().copied().unwrap_or("")
    }

    /// The manifest action this row projects, when it projects one.
    pub fn source_target_id(&self) -> Option<ActionId> {
        match self.source {
            RouteSource::Manifest(id) => Some(id),
            RouteSource::ClientOnly { .. } => None,
        }
    }
}

/// Build a route whose action ignores the draft.
fn to_dashboard(_text: String) -> InputAction {
    InputAction::NavigateDashboard
}
fn to_settings(_text: String) -> InputAction {
    InputAction::NavigateSettings
}
fn to_new_thread(_text: String) -> InputAction {
    InputAction::CreateNewSession
}
fn to_close_tab(_text: String) -> InputAction {
    InputAction::CloseTab
}
fn to_sessions(_text: String) -> InputAction {
    InputAction::OpenSessions
}
fn to_models(_text: String) -> InputAction {
    InputAction::OpenModels
}
fn to_connections(_text: String) -> InputAction {
    InputAction::OpenConnections
}
fn to_permissions(_text: String) -> InputAction {
    InputAction::OpenPermissions
}
fn to_tools(_text: String) -> InputAction {
    InputAction::OpenTools
}
fn to_usage(_text: String) -> InputAction {
    InputAction::OpenUsage
}
fn to_quotas(_text: String) -> InputAction {
    InputAction::OpenQuotas
}
fn to_stats(_text: String) -> InputAction {
    InputAction::OpenThreadStats
}
fn to_trace(_text: String) -> InputAction {
    InputAction::OpenSessionTrace
}
fn to_context_plan(_text: String) -> InputAction {
    InputAction::OpenContextPlan
}
fn to_mcp(_text: String) -> InputAction {
    InputAction::OpenMcp
}
fn to_skills(_text: String) -> InputAction {
    InputAction::OpenSkills
}
fn to_notices(_text: String) -> InputAction {
    InputAction::OpenNotices
}
fn to_port_palette(_text: String) -> InputAction {
    InputAction::OpenPortPalette
}
fn to_help(_text: String) -> InputAction {
    InputAction::OpenHelp
}
fn to_quit(_text: String) -> InputAction {
    InputAction::Quit
}

/// A row the client owns because it is workspace navigation, not a thread action.
const fn client(
    spellings: &'static [&'static str],
    target: fn(String) -> InputAction,
    reason: &'static str,
) -> Route {
    Route {
        spellings,
        target: RouteTarget::Fixed(target),
        args: ArgsPolicy::Any,
        source: RouteSource::ClientOnly { reason },
    }
}

/// A row that projects a composition the server's manifest declares.
const fn manifest_row(
    spellings: &'static [&'static str],
    target: fn(String) -> InputAction,
    id: &'static str,
) -> Route {
    Route {
        spellings,
        target: RouteTarget::Fixed(target),
        args: ArgsPolicy::Any,
        source: RouteSource::Manifest(ActionId(id)),
    }
}

/// The routing table.
///
/// Ordered by the three sources, which is also the order of the reasoning: what the
/// manifest declares, what the client owns and the server must not know, and what crosses
/// the wire with no local surface.
pub static ROUTES: &[Route] = &[
    // ── Declared by the manifest, presented by the client ──────────────────────────
    // `Alias` spellings sit beside the canonical one, because a retired name must not
    // survive in a place this table cannot see.
    //
    // Bare `/new` / `/thread` triggers the zero-arg client action; `/new <prompt>`
    // carries an opening prompt and must reach `handle_send_slash` verbatim so the
    // prompt argument is never silently dropped (ADR-0252 §4).
    Route {
        spellings: &["/new", "/thread"],
        target: RouteTarget::Fixed(to_new_thread),
        args: ArgsPolicy::ExactOnly,
        source: RouteSource::Manifest(ActionId("thread.start_fresh")),
    },
    manifest_row(&["/exit"], to_quit, "thread.end_foreground"),
    // ── Client-owned workspace navigation (ADR-0041 `[INV-CMD-01]`) ────────────────
    client(
        &["/dashboard"],
        to_dashboard,
        "workspace view, not thread-scoped: ADR-0041 [INV-CMD-01]",
    ),
    client(
        &["/settings", "/config"],
        to_settings,
        "workspace view, not thread-scoped: ADR-0041 [INV-CMD-01]",
    ),
    client(
        &["/close"],
        to_close_tab,
        "viewport action: closing a tab is a detach, not a thread operation (ADR-0038)",
    ),
    // ── Client surfaces over server-held state ────────────────────────────────────
    client(
        &["/threads", "/sessions"],
        to_sessions,
        "the picker is a client dialog; the rows come from a query (ADR-0128)",
    ),
    client(
        &["/models"],
        to_models,
        "the server owns the state, the client owns the picker surface",
    ),
    client(
        &["/connections"],
        to_connections,
        "the server owns the state, the client owns the surface",
    ),
    client(
        &["/permissions"],
        to_permissions,
        "the server owns the rules, the client owns the manager surface",
    ),
    client(
        &["/tools"],
        to_tools,
        "the server owns the registry, the client owns the surface",
    ),
    client(
        &["/usage"],
        to_usage,
        "a reading projected into a client overlay (ADR-0140)",
    ),
    client(
        &["/quota", "/quotas"],
        to_quotas,
        "a reading projected into a client overlay",
    ),
    client(
        &["/stats"],
        to_stats,
        "a reading projected into a client overlay",
    ),
    client(
        &["/trace"],
        to_trace,
        "a reading projected into a client overlay",
    ),
    client(
        &["/mcp"],
        to_mcp,
        "the server owns the registry, the client owns the surface",
    ),
    client(
        &["/skills"],
        to_skills,
        "the server owns the catalog, the client owns the surface",
    ),
    client(
        &["/notices", "/logs"],
        to_notices,
        "the notice log is client-resident by construction (ADR-0121)",
    ),
    client(
        &["/port", "/ports", "/share"],
        to_port_palette,
        "endpoint inspection is a client palette over a control-plane query",
    ),
    client(
        &["/help"],
        to_help,
        "the keymap is client state; the server cannot describe it (ADR-0131)",
    ),
    // ── A reading whose argument changes the question ─────────────────────────────
    Route {
        spellings: &["/context"],
        target: RouteTarget::Fixed(to_context_plan),
        // Bare `/context` is the inspector. Every variant — `--json`, or any future
        // subcommand — stays a slash command: the machine form is rendered by the server,
        // and a client that swallowed the argument would answer a different question
        // (ADR-0220 §7).
        args: ArgsPolicy::ExactOnly,
        source: RouteSource::ClientOnly {
            reason: "bare /context opens the inspector; with arguments the machine form is \
                     the server's to render (ADR-0220 §7)",
        },
    },
    // ── No client surface: forwarded by absence ───────────────────────────────────
    // `/compact`, `/undo`, `/fork`, `/role`, `/trust`, `/search`, … carry no client
    // surface and are dispatched by the server. They are deliberately **not listed**: an
    // unclaimed spelling is forwarded verbatim, which is what ADR-0119 §2 established
    // (admission belongs to the dispatcher, never to the frontend). Listing them would
    // recreate the second copy this module exists to remove, and a row for one would need
    // a target that does nothing — the dead-variant shape ADR-0179 forbids.
];

/// The route for a draft, or `None` when nothing claims it.
///
/// `whole_draft` is the trimmed text; `cmd` is its first token. Two parameters rather than
/// one because [`ArgsPolicy::ExactOnly`] compares them, and deriving `cmd` here would make
/// the caller's own split and this one able to disagree.
pub fn route_for(cmd: &str, whole_draft: &str) -> Option<&'static Route> {
    ROUTES
        .iter()
        .find(|route| route.spellings.contains(&cmd) && accepts(route, whole_draft))
}

fn accepts(route: &Route, whole_draft: &str) -> bool {
    match route.args {
        ArgsPolicy::Any => true,
        ArgsPolicy::ExactOnly => route.spellings.contains(&whole_draft),
    }
}

/// Resolve a `/`-prefixed draft into the action it means.
///
/// A spelling no row claims is forwarded verbatim: the dispatcher is the single admission
/// point (ADR-0119 §2), so a typo is rejected by the authority rather than reinterpreted
/// here.
pub fn resolve(text: &str) -> InputAction {
    let trimmed = text.trim();
    let cmd = trimmed.split_once(' ').map_or(trimmed, |(cmd, _)| cmd);
    match route_for(cmd, trimmed).map(|route| route.target) {
        Some(RouteTarget::Fixed(build)) => build(text.to_string()),
        None => InputAction::SendSlash(text.to_string()),
    }
}

#[cfg(test)]
pub mod tests;
