//! The palette's vocabulary: what an entry is, where it groups, what it does.

use std::borrow::Cow;

use crate::shell::input::InputAction;
use crate::shell::keymap::{Availability, CommandId, DangerLevel, Key};
use crate::shell::surfaces::{DialogKind, SceneKind};

/// Where an entry is grouped. Declaration order is the grouped view's order:
/// the surface the user is looking at first, then the rest of the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Section {
    /// Verbs of the dialog the palette was opened over.
    Dialog(DialogKind),
    /// Verbs of the current scene.
    Scene(SceneKind),
    /// Presentation-only group for recently run entries (grouped view).
    Recent,
    /// Other open tabs.
    Tabs,
    /// Destinations: scenes and dialogs to open.
    GoTo,
    /// Operations on the current session.
    Session,
    /// Bulk and management actions.
    Actions,
    /// Client lifecycle and housekeeping.
    App,
}

impl Section {
    /// Sort rank for the grouped view.
    pub(crate) fn rank(self) -> u8 {
        match self {
            Section::Dialog(_) => 0,
            Section::Scene(_) => 1,
            Section::Recent => 2,
            Section::Tabs => 3,
            Section::GoTo => 4,
            Section::Session => 5,
            Section::Actions => 6,
            Section::App => 7,
        }
    }

    /// Whether this section describes the surface under the palette.
    pub(crate) fn is_here(self) -> bool {
        matches!(self, Section::Dialog(_) | Section::Scene(_))
    }

    /// Whether entries here are places to go rather than things to do.
    pub(crate) fn is_place(self) -> bool {
        matches!(self, Section::Tabs | Section::GoTo)
    }

    pub(crate) fn title(self) -> &'static str {
        match self {
            Section::Dialog(kind) => kind.label(),
            Section::Scene(kind) => kind.label(),
            Section::Recent => "Recent",
            Section::Tabs => "Tabs",
            Section::GoTo => "Go to",
            Section::Session => "Session",
            Section::Actions => "Actions",
            Section::App => "App",
        }
    }
}

/// What running an entry does. The palette implements no verb itself
/// (`[INV-PAL-CTX-03]`): it names a registry command or the exact action a
/// key produces, and the ordinary dispatcher does the rest.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Effect {
    Command(CommandId),
    Action(InputAction),
}

/// One runnable row.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    /// Stable identity, independent of the title: recency and tests key on it.
    pub key: Cow<'static, str>,
    /// Matched and highlighted against the query.
    pub title: Cow<'static, str>,
    /// One line of guidance shown for the selected entry.
    pub detail: &'static str,
    /// Extra match terms (synonyms, slash forms); never highlighted.
    pub keywords: &'static str,
    pub section: Section,
    /// The effective chord that runs this entry outside the palette.
    pub keycap: Option<Key>,
    /// A typed alternative (`/stats`) shown when there is no chord.
    pub alias: Option<&'static str>,
    pub danger: DangerLevel,
    pub availability: Availability,
    pub effect: Effect,
}

impl Entry {
    pub(crate) fn is_available(&self) -> bool {
        matches!(self.availability, Availability::Available)
    }
}
