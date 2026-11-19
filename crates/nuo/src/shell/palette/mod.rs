//! The contextual command palette (RFC-0005).
//!
//! A pure projection, `context → sources → entries → rank → rows`:
//!
//! - [`context`] reads `App` once ([`PaletteContext::capture`]); nothing
//!   downstream sees `App`.
//! - [`source`] lists the entries that are relevant there: the dialog under
//!   the palette, the current scene, other tabs, and registry commands.
//! - [`verbs`] declares each scene's and dialog's verbs as the exact
//!   `InputAction` their key produces. The in-dialog key reference reads the
//!   same tables.
//! - [`query`] parses facet sigils; [`rank`] orders and groups.
//! - [`view`] draws a [`Projection`].
//!
//! Render, list navigation, pointer hits and execution all call [`project`],
//! so the row the user sees highlighted is the row Enter runs
//! (`[INV-PAL-CTX-01]`).

pub(crate) mod context;
pub(crate) mod entry;
pub(crate) mod query;
pub(crate) mod rank;
pub(crate) mod recents;
pub(crate) mod source;
pub(crate) mod verbs;
pub(crate) mod view;

#[cfg(test)]
mod tests;

pub(crate) use context::PaletteContext;
pub(crate) use entry::Effect;
pub(crate) use rank::Projection;
pub(crate) use recents::Recents;

use crate::shell::App;

/// Project the palette for `app` and `query`.
///
/// The query is passed rather than read from the dialog entity because
/// callers often hold that entity detached from `App` while they work.
pub(crate) fn project(app: &App, query: &str) -> Projection {
    project_with(
        &PaletteContext::capture(app),
        query,
        &app.surfaces.palette_recents,
    )
}

/// The pure core of [`project`].
pub(crate) fn project_with(ctx: &PaletteContext, query: &str, recents: &Recents) -> Projection {
    let query = query::Query::parse(query);
    let entries = rank::rank(source::collect(ctx), &query, recents);
    let rows = rank::rows(&entries, query.is_browse());
    Projection {
        here: ctx.here(),
        entries,
        rows,
    }
}
