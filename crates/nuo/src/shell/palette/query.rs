//! Query parsing: an optional facet sigil, then a free-text needle.

use super::entry::Section;

/// Which slice of the entries a query searches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Facet {
    All,
    /// `:` What the surface under the palette can do.
    Here,
    /// `@` Places to go: tabs and destinations.
    Places,
    /// `>` Everything that is a verb rather than a place.
    Actions,
}

impl Facet {
    /// The sigils, in the order the palette advertises them.
    pub(crate) const SIGILS: [(char, Facet, &'static str); 3] = [
        (':', Facet::Here, "here"),
        ('@', Facet::Places, "places"),
        ('>', Facet::Actions, "actions"),
    ];

    pub(crate) fn admits(self, section: Section) -> bool {
        match self {
            Facet::All => true,
            Facet::Here => section.is_here(),
            Facet::Places => section.is_place(),
            Facet::Actions => !section.is_place(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Query<'a> {
    pub facet: Facet,
    /// Trimmed search text; empty means "browse".
    pub needle: &'a str,
}

impl<'a> Query<'a> {
    pub(crate) fn parse(text: &'a str) -> Self {
        let text = text.trim_start();
        let mut chars = text.chars();
        if let Some(first) = chars.next()
            && let Some(&(_, facet, _)) = Facet::SIGILS.iter().find(|(c, ..)| *c == first)
        {
            return Self {
                facet,
                needle: chars.as_str().trim(),
            };
        }
        Self {
            facet: Facet::All,
            needle: text.trim(),
        }
    }

    pub(crate) fn is_browse(&self) -> bool {
        self.needle.is_empty()
    }
}
