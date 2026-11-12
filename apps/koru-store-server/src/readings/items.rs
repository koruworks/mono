// SPDX-License-Identifier: MPL-2.0

//! **The items behind the Settings row's *could not be read* count** — the
//! answer to `GET /v1/readings/reader/items` (OCR Plan 2.5).
//!
//! The coordinator publishes counts by reason; a person reading "3 could not
//! be read" wants to know which three. This answers that from the same three
//! read models the coordinator's queue is computed from — the projection's
//! readable references, the mirror's outcomes, the local attempts table — on
//! connections of its own, with **no Store lock and no new query**: every
//! statement here is one the coordinator already runs
//! (`Projections::listings`, `readable_references`, `Mirror::outcomes_in`,
//! `Mirror::attempts`), so their query plans are the ones already recorded.
//!
//! **Its own module, beside the coordinator rather than inside it**, so a
//! change to how the queue is ordered or batched (Plan 3.1) and this list do
//! not edit the same lines. What they must agree on is what *failed* means,
//! and that is not restated here: [`failure`] asks the mirror's
//! [`Outcome::settled`] and the coordinator's own [`verdict`], the two
//! functions `Coordinator::classify` counts by.
//!
//! **Ids and tokens only.** A file's name lives in the workspace listing, and
//! a surface reads that listing already.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use koru_projection::{Projections, ReadableReference, ReadableScope};
use koru_read::READER_TEXT;
use koru_store_core::ids::{DocumentId, WorkspaceId};
use koru_store_wire::reader::{READER_ITEMS_LIMIT, ReaderItem, ReaderItemKind, ReaderItems};

use super::coordinator::{Verdict, readable, verdict};
use super::mirror::{Attempt, Mirror, Outcome, Settled};
use crate::derived::Derived;

/// The read models this list is computed from, opened on first use and kept,
/// behind a lock nothing else takes.
pub(crate) struct ItemsSource {
    derived: Derived,
    open: Mutex<Option<(Projections, Mirror)>>,
}

impl ItemsSource {
    pub(crate) fn new(derived: Derived) -> Self {
        Self {
            derived,
            open: Mutex::new(None),
        }
    }

    /// Every item on this computer that could not be read, workspace by
    /// workspace. `Err` carries SQLite's message, which names tables and
    /// never a row's value.
    pub(crate) fn items(&self, now: i64) -> Result<ReaderItems, String> {
        let mut open = self
            .open
            .lock()
            .map_err(|_| "the readings items' lock is poisoned".to_owned())?;
        if open.is_none() {
            let projections = self.derived.projections().map_err(|e| e.0)?;
            let mirror = self.derived.mirror().map_err(|e| e.0)?;
            *open = Some((projections, mirror));
        }
        let (projections, mirror) = open.as_ref().expect("opened above");
        let attempts = mirror.attempts(READER_TEXT).map_err(|e| e.0)?;
        let mut workspaces = Vec::new();
        for (workspace, _) in projections.listings().map_err(|e| e.0)? {
            let references = projections
                .readable_references(workspace, ReadableScope::Workspace)
                .map_err(|e| e.0)?;
            if references.is_empty() {
                continue;
            }
            let outcomes = mirror.outcomes_in(workspace).map_err(|e| e.0)?;
            workspaces.push((workspace, references, outcomes));
        }
        Ok(collect(workspaces, &attempts, now))
    }
}

/// The list, from what the read models hold. Pure, so every case is tested
/// without a database.
pub(crate) fn collect(
    workspaces: Vec<(
        WorkspaceId,
        Vec<ReadableReference>,
        HashMap<(DocumentId, String), Outcome>,
    )>,
    attempts: &HashMap<String, Attempt>,
    now: i64,
) -> ReaderItems {
    let mut answer = ReaderItems::default();
    for (workspace, references, outcomes) in workspaces {
        // Two nodes of one page holding the same picture are one item, as
        // they are one in the count.
        let mut seen: HashSet<(DocumentId, String)> = HashSet::new();
        for reference in references {
            if !seen.insert((reference.document, reference.blob.clone())) {
                continue;
            }
            let Some(reason) = failure(&reference, &outcomes, attempts, now) else {
                continue;
            };
            if answer.items.len() >= READER_ITEMS_LIMIT {
                answer.truncated = true;
                return answer;
            }
            answer.items.push(ReaderItem {
                workspace,
                document: reference.document,
                hash: reference.blob,
                kind: ReaderItemKind::CouldNotBeRead,
                reason,
            });
        }
    }
    answer
}

/// Why one reference could not be read, or `None` when it was read, is still
/// to be read, or is not something the queue reads at all — the coordinator's
/// own classification, asked in the order `Coordinator::classify` asks it.
pub(crate) fn failure(
    reference: &ReadableReference,
    outcomes: &HashMap<(DocumentId, String), Outcome>,
    attempts: &HashMap<String, Attempt>,
    now: i64,
) -> Option<String> {
    if reference.trashed {
        return None;
    }
    // A media type the queue would not send is not an item at all.
    if reference
        .mime
        .as_deref()
        .is_some_and(|mime| readable(mime).is_none())
    {
        return None;
    }
    if let Some(settled) = outcomes
        .get(&(reference.document, reference.blob.clone()))
        .and_then(Outcome::settled)
    {
        return match settled {
            Settled::Failed(reason) => Some(reason),
            Settled::Read | Settled::PartlyRead => None,
        };
    }
    let attempt = attempts.get(&reference.blob)?;
    (verdict(attempt, now) == Verdict::Failed).then(|| attempt.code.as_str().to_owned())
}

#[cfg(test)]
mod tests;
