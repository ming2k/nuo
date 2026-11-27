//! Ranking: entries + query + recency → an ordered, row-addressable projection.
//!
//! Deterministic and pure (RFC-0005 §2.6). Two modes:
//!
//! - **Browse** (empty needle): grouped by section in declaration order, with
//!   up to [`RECENT_LIMIT`] recently run entries lifted into a `Recent` group.
//!   Within a group, runnable entries precede blocked ones. Curated table order
//!   is kept: alphabetising it would throw the curation away.
//! - **Search**: flat. Title matches score and highlight; keyword and detail
//!   matches are a penalised fallback with no highlight. Runnable entries come
//!   first, then score, then a recency and a context bonus.

use nuo_ui_primitives::fuzzy::fuzzy_match;

use super::entry::{Entry, Section};
use super::query::Query;
use super::recents::Recents;

/// How many recent entries the browse view lifts into its `Recent` group.
pub(crate) const RECENT_LIMIT: usize = 5;

/// Penalty for a match found only in keywords or detail text.
const SECONDARY_PENALTY: i64 = 400;
/// Bonus for the surface the user is on.
const CONTEXT_BONUS: i64 = 30;
/// Bonus per recency slot (most recent earns `RECENT_BONUS * RECENT_LIMIT`).
const RECENT_BONUS: i64 = 15;

/// One ranked entry.
#[derive(Debug, Clone)]
pub(crate) struct Ranked {
    pub entry: Entry,
    /// The group it is displayed under (its section, or `Recent`).
    pub group: Section,
    /// Char indices of `entry.title` that the needle matched.
    pub positions: Vec<usize>,
}

/// A displayed row: a non-selectable group header or an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Row {
    Header(Section),
    /// Index into [`Projection::entries`].
    Entry(usize),
}

/// The palette's whole output for one context and query.
#[derive(Debug, Clone)]
pub(crate) struct Projection {
    /// Where the user stands (`Thread › Models`).
    pub here: String,
    pub entries: Vec<Ranked>,
    pub rows: Vec<Row>,
}

impl Projection {
    /// The row that displays entry `index`.
    pub(crate) fn row_of(&self, index: usize) -> Option<usize> {
        self.rows.iter().position(|row| *row == Row::Entry(index))
    }

    /// The entry displayed at `row`, if it is not a header.
    pub(crate) fn entry_at_row(&self, row: usize) -> Option<usize> {
        match self.rows.get(row)? {
            Row::Entry(index) => Some(*index),
            Row::Header(_) => None,
        }
    }

    /// The entry Enter would run: the selection, clamped to the list.
    pub(crate) fn selected(&self, selected: usize) -> Option<&Ranked> {
        self.entries.get(selected).or_else(|| self.entries.first())
    }
}

pub(crate) fn rank(entries: Vec<Entry>, query: &Query<'_>, recents: &Recents) -> Vec<Ranked> {
    let admitted = entries
        .into_iter()
        .enumerate()
        .filter(|(_, entry)| query.facet.admits(entry.section));
    if query.is_browse() {
        browse(admitted, recents)
    } else {
        search(admitted, query.needle, recents)
    }
}

fn browse(entries: impl Iterator<Item = (usize, Entry)>, recents: &Recents) -> Vec<Ranked> {
    let mut keyed: Vec<((u8, bool, usize, usize), Ranked)> = entries
        .map(|(order, entry)| {
            let recent = recents
                .position(&entry.key)
                .filter(|pos| *pos < RECENT_LIMIT && !entry.section.is_here());
            let group = if recent.is_some() {
                Section::Recent
            } else {
                entry.section
            };
            let key = (
                group.rank(),
                !entry.is_available(),
                recent.unwrap_or(0),
                order,
            );
            (
                key,
                Ranked {
                    entry,
                    group,
                    positions: Vec::new(),
                },
            )
        })
        .collect();
    keyed.sort_by_key(|(key, _)| *key);
    keyed.into_iter().map(|(_, ranked)| ranked).collect()
}

fn search(
    entries: impl Iterator<Item = (usize, Entry)>,
    needle: &str,
    recents: &Recents,
) -> Vec<Ranked> {
    let mut scored: Vec<((bool, i64, u8, usize), Ranked)> = entries
        .filter_map(|(order, entry)| {
            let (mut score, positions) = match fuzzy_match(&entry.title, needle) {
                Some(hit) => (hit.score, hit.positions),
                None => {
                    let secondary = [entry.keywords, entry.alias.unwrap_or(""), entry.detail]
                        .into_iter()
                        .filter(|text| !text.is_empty())
                        .filter_map(|text| fuzzy_match(text, needle))
                        .map(|hit| hit.score)
                        .max()?;
                    (secondary - SECONDARY_PENALTY, Vec::new())
                }
            };
            if let Some(pos) = recents.position(&entry.key).filter(|p| *p < RECENT_LIMIT) {
                score += RECENT_BONUS * (RECENT_LIMIT - pos) as i64;
            }
            if entry.section.is_here() {
                score += CONTEXT_BONUS;
            }
            let key = (!entry.is_available(), -score, entry.section.rank(), order);
            Some((
                key,
                Ranked {
                    group: entry.section,
                    entry,
                    positions,
                },
            ))
        })
        .collect();
    scored.sort_by_key(|(key, _)| *key);
    scored.into_iter().map(|(_, ranked)| ranked).collect()
}

/// Lay out rows: grouped (with headers) when browsing, flat when searching.
pub(crate) fn rows(entries: &[Ranked], grouped: bool) -> Vec<Row> {
    let mut rows = Vec::with_capacity(entries.len() + 8);
    let mut current: Option<Section> = None;
    for (index, ranked) in entries.iter().enumerate() {
        if grouped && current != Some(ranked.group) {
            rows.push(Row::Header(ranked.group));
            current = Some(ranked.group);
        }
        rows.push(Row::Entry(index));
    }
    rows
}
