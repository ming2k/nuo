//! Entry sources. Each is a pure function of the context that appends the
//! entries that are **relevant** there (RFC-0005 §2.2): an irrelevant entry is
//! absent, a relevant-but-blocked one is present with its reason. Supporting a
//! new kind of entry means adding a source here, not changing the ranker or
//! the view.

use std::borrow::Cow;

use crate::shell::input::InputAction;
use crate::shell::keymap::{
    Availability, COMMAND_REGISTRY, CommandCategory, CommandId, DangerLevel, DisclosurePriority,
    Key, KeyCode, KeyModifiers, Scope,
};
use crate::shell::surfaces::SceneKind;

use super::context::PaletteContext;
use super::entry::{Effect, Entry, Section};
use super::verbs::{Verb, dialog_verbs, scene_verbs};

type Source = fn(&PaletteContext, &mut Vec<Entry>);

/// Every source, in the order their entries are declared. The order is the
/// tie-break of last resort, so it is part of the curated presentation.
const SOURCES: &[Source] = &[dialog, scene, tabs, registry];

/// Collect every relevant entry for `ctx`.
pub(crate) fn collect(ctx: &PaletteContext) -> Vec<Entry> {
    let mut out = Vec::with_capacity(96);
    for source in SOURCES {
        source(ctx, &mut out);
    }
    out
}

fn verb_entry(verb: &'static Verb, section: Section, ctx: &PaletteContext) -> Entry {
    Entry {
        key: Cow::Borrowed(verb.key),
        title: Cow::Borrowed(verb.title),
        detail: verb.detail,
        keywords: verb.keywords,
        section,
        keycap: verb.effective_keycap(ctx),
        alias: None,
        danger: verb.danger,
        availability: verb.requires.check(ctx),
        effect: Effect::Action(verb.action.clone()),
    }
}

/// The verbs of the dialog the palette was opened over.
fn dialog(ctx: &PaletteContext, out: &mut Vec<Entry>) {
    if let Some(kind) = ctx.dialog() {
        out.extend(
            dialog_verbs(kind)
                .iter()
                .map(|verb| verb_entry(verb, Section::Dialog(kind), ctx)),
        );
    }
}

/// The current scene's verbs. Behind a dialog they still describe the page
/// the user will return to, but they run against the scene, so they are only
/// offered when nothing else is open.
fn scene(ctx: &PaletteContext, out: &mut Vec<Entry>) {
    if ctx.state.has_overlay {
        return;
    }
    let kind = ctx.scene();
    for group in scene_verbs(kind) {
        out.extend(
            group
                .iter()
                .map(|verb| verb_entry(verb, Section::Scene(kind), ctx)),
        );
    }
}

/// Every other open tab, reachable by ordinal.
fn tabs(ctx: &PaletteContext, out: &mut Vec<Entry>) {
    if ctx.tabs.len() < 2 {
        return;
    }
    for tab in ctx.tabs.iter().filter(|tab| !tab.active) {
        let keycap = (tab.index < 9).then(|| Key {
            modifiers: KeyModifiers::ALT,
            code: KeyCode::Char(char::from(b'1' + tab.index as u8)),
        });
        out.push(Entry {
            key: Cow::Owned(tab.key.clone()),
            title: Cow::Owned(tab.label.clone()),
            detail: "Switch to this tab",
            keywords: "tab switch",
            section: Section::Tabs,
            keycap,
            alias: None,
            danger: DangerLevel::Safe,
            availability: Availability::Available,
            effect: Effect::Action(InputAction::SelectTab(tab.index)),
        });
    }
}

/// Registry commands with palette disclosure.
fn registry(ctx: &PaletteContext, out: &mut Vec<Entry>) {
    for spec in COMMAND_REGISTRY.iter().filter(|spec| {
        matches!(spec.scope, Scope::Global | Scope::Session)
            && spec.disclosure == DisclosurePriority::L2Palette
    }) {
        let section = match spec.category {
            CommandCategory::Navigate | CommandCategory::Settings => Section::GoTo,
            CommandCategory::Session => Section::Session,
            CommandCategory::Actions => Section::Actions,
            CommandCategory::Global => Section::App,
        };
        let availability = match here(spec.id) {
            Some(scene) if scene == ctx.scene() && !ctx.state.has_overlay => {
                Availability::Unavailable("you are here")
            }
            _ => (spec.availability)(&ctx.state),
        };
        out.push(Entry {
            key: Cow::Owned(format!("cmd.{:?}", spec.id)),
            title: Cow::Borrowed(spec.label),
            detail: spec.description,
            keywords: spec.slash.unwrap_or(""),
            section,
            keycap: ctx
                .global_keys
                .effective_binding(spec.id)
                .or_else(|| spec.bindings.first().copied()),
            alias: spec.slash,
            danger: spec.danger,
            availability,
            effect: Effect::Command(spec.id),
        });
    }
}

/// The scene a navigation command leads to, so "go to where you already are"
/// can say so instead of doing nothing.
fn here(id: CommandId) -> Option<SceneKind> {
    match id {
        CommandId::NavigateSession => Some(SceneKind::Thread),
        CommandId::NavigateDashboard => Some(SceneKind::Dashboard),
        CommandId::NavigateSettings => Some(SceneKind::Settings),
        _ => None,
    }
}
