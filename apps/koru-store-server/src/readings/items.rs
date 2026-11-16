// SPDX-License-Identifier: MPL-2.0

//! **The items behind the Settings row's *could not be read* and *partly
//! read* lists** — the answer to `GET /v1/readings/reader/items` (OCR Plans
//! 2.5 and 3.5).
//!
//! The coordinator publishes counts; a person reading "3 could not be read"
//! wants to know which three, and a person with a long scan wants *Read all
//! pages* beside it (EXPERIENCE §4). This answers both from the same read
//! models the coordinator's queue is computed from — the projection's
//! readable references, the mirror's outcomes, the local attempts table, the
//! requests — on connections of its own, with **no new query**: every
//! statement here is one the coordinator already runs
//! (`Projections::listings`, `readable_references`, `Mirror::outcomes_in`,
//! `Mirror::attempts`, `Mirror::requests_in`), so their query plans are the
//! ones already recorded.
//!
//! **Its own module, beside the coordinator rather than inside it**, so a
//! change to how the queue is ordered or batched (Plan 3.1) and this list do
//! not edit the same lines. What they must agree on is what *failed* and
//! *partly read* mean, and that is not restated here: [`classify`] asks the
//! mirror's [`Outcome::settled`] and the coordinator's own [`verdict`], the
//! two functions `Coordinator::classify` counts by.
//!
//! **The offers are the menu's** (OCR Plan 3.5): each listed item's
//! [`Facts`](super::Facts) — the decision `GET /v1/readings/{workspace}/{hash}`
//! makes for a right-click menu — so the list offers exactly what the menu
//! offers. Its entry's own mirror row stands in for the reading document, as
//! the reading route's fall-back does; the Workspace's blob row and the disk
//! are asked by the caller, only for the items listed.
//!
//! **Ids and tokens only.** A file's name lives in the workspace listing, and
//! a surface reads that listing already.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use koru_projection::{Projections, ReadableReference, ReadableScope};
use koru_read::READER_TEXT;
use koru_store_core::ids::{DocumentId, WorkspaceId};
use koru_store_wire::reader::{READER_ITEMS_LIMIT, ReaderItem, ReaderItemKind, ReaderItems};

use super::Facts;
use super::coordinator::{Verdict, readable, verdict};
use super::mirror::{Attempt, Mirror, Outcome, Settled};
use crate::derived::Derived;

/// Where one listed item's bytes are, as the reading route asks it: the
/// Workspace's blob row's media type (`None`: it has no row, so it may not
/// read them), and whether the object is on this computer.
pub(crate) type Place<'a> = dyn FnMut(WorkspaceId, &str) -> (Option<String>, bool) + 'a;

/// One workspace's read models, as [`collect`] takes them.
pub(crate) struct Held {
    pub workspace: WorkspaceId,
    pub references: Vec<ReadableReference>,
    pub outcomes: HashMap<(DocumentId, String), Outcome>,
    /// The hashes a person asked for here.
    pub requests: HashSet<String>,
}

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

    fn with<T>(
        &self,
        f: impl FnOnce(&Projections, &Mirror) -> Result<T, String>,
    ) -> Result<T, String> {
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
        f(projections, mirror)
    }

    /// Every item on this computer that could not be read or was partly
    /// read, workspace by workspace. `Err` carries SQLite's message, which
    /// names tables and never a row's value.
    pub(crate) fn items(
        &self,
        now: i64,
        reader_ready: bool,
        place: &mut Place<'_>,
    ) -> Result<ReaderItems, String> {
        let (held, attempts) = self.with(|projections, mirror| {
            let attempts = mirror.attempts(READER_TEXT).map_err(|e| e.0)?;
            let mut held = Vec::new();
            for (workspace, _) in projections.listings().map_err(|e| e.0)? {
                let references = projections
                    .readable_references(workspace, ReadableScope::Workspace)
                    .map_err(|e| e.0)?;
                if references.is_empty() {
                    continue;
                }
                held.push(Held {
                    workspace,
                    references,
                    outcomes: mirror.outcomes_in(workspace).map_err(|e| e.0)?,
                    requests: mirror
                        .requests_in(workspace)
                        .map_err(|e| e.0)?
                        .into_iter()
                        .collect(),
                });
            }
            Ok((held, attempts))
        })?;
        // The blob rows and the disk are asked with the read models let go.
        Ok(collect(held, &attempts, now, reader_ready, place))
    }

    /// **The live entry that holds `hash` in `workspace`** — the first by id,
    /// as the coordinator picks a request's carrier — for an item that has no
    /// reading to name its entry (OCR Plan 3.5). `None` when no live entry
    /// holds it.
    pub(crate) fn holder(
        &self,
        workspace: WorkspaceId,
        hash: &str,
    ) -> Result<Option<DocumentId>, String> {
        self.with(|projections, _| {
            let references = projections
                .readable_references(workspace, ReadableScope::Carrying(&[hash.to_owned()]))
                .map_err(|e| e.0)?;
            Ok(references
                .into_iter()
                .filter(|reference| !reference.trashed && reference.blob == hash)
                .map(|reference| reference.document)
                .min_by_key(DocumentId::to_storage))
        })
    }
}

/// The two lists, from what the read models hold. Pure but for `place`, so
/// every case is tested without a database.
pub(crate) fn collect(
    held: Vec<Held>,
    attempts: &HashMap<String, Attempt>,
    now: i64,
    reader_ready: bool,
    place: &mut Place<'_>,
) -> ReaderItems {
    let mut answer = ReaderItems::default();
    for Held {
        workspace,
        references,
        outcomes,
        requests,
    } in held
    {
        // Two nodes of one page holding the same picture are one item, as
        // they are one in the count.
        let mut seen: HashSet<(DocumentId, String)> = HashSet::new();
        for reference in references {
            if !seen.insert((reference.document, reference.blob.clone())) {
                continue;
            }
            let outcome = outcomes.get(&(reference.document, reference.blob.clone()));
            let attempt = attempts.get(&reference.blob);
            let requested = requests.contains(&reference.blob);
            let Some(listed) = classify(&reference, outcome, attempt, requested, now) else {
                continue;
            };
            if answer.items.len() >= READER_ITEMS_LIMIT {
                answer.truncated = true;
                return answer;
            }
            let (mime, here) = place(workspace, &reference.blob);
            let facts = Facts {
                status: outcome.and_then(|o| o.status.clone()),
                reason: outcome.and_then(|o| o.reason.clone()),
                source: outcome.and_then(|o| o.source.clone()),
                reader: None,
                mime,
                here,
                reader_ready,
                pending: requested,
                failed_here: attempt.is_some_and(|a| verdict(a, now) == Verdict::Failed),
            };
            let offers = facts.offers();
            answer.items.push(ReaderItem {
                workspace,
                document: reference.document,
                hash: reference.blob,
                kind: listed.kind,
                reason: listed.reason,
                pages_done: listed.pages_done,
                // Each list offers its own action: the other is the menu's.
                offers: match listed.kind {
                    ReaderItemKind::PartlyRead => koru_store_wire::readings::ReadingOffers {
                        force_ocr: false,
                        read_all: offers.read_all,
                        read_again: false,
                    },
                    ReaderItemKind::CouldNotBeRead => koru_store_wire::readings::ReadingOffers {
                        force_ocr: false,
                        read_all: false,
                        read_again: offers.read_again,
                    },
                },
                requested,
            });
        }
    }
    answer
}

/// Which list one reference is on, and what the row says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Listed {
    pub kind: ReaderItemKind,
    pub reason: String,
    pub pages_done: Option<u32>,
}

/// **Which list one reference belongs on**, or `None` when it was read, is
/// still to be read, or is not something the queue reads at all — the
/// coordinator's own classification, asked in the order
/// `Coordinator::classify` asks it.
///
/// - *Could not be read*: settled `failed` (a reason true on every device), or
///   no settled reading and the local attempts table says this computer gave
///   up on it.
/// - *Partly read*: settled `partial` (stopped at a page limit), **or a
///   person's request for it is under way** — its reading a `partial` with no
///   reason, a window at a time — so the row says how far it has got and
///   leaves the list when it finishes, as EXPERIENCE §9 says, rather than the
///   moment the first window lands.
pub(crate) fn classify(
    reference: &ReadableReference,
    outcome: Option<&Outcome>,
    attempt: Option<&Attempt>,
    requested: bool,
    now: i64,
) -> Option<Listed> {
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
    let pages = |outcome: &Outcome| u32::try_from(outcome.page_count).ok();
    if let Some(outcome) = outcome {
        match outcome.settled() {
            Some(Settled::Failed(reason)) => {
                return Some(Listed {
                    kind: ReaderItemKind::CouldNotBeRead,
                    reason,
                    pages_done: None,
                });
            }
            Some(Settled::PartlyRead) => {
                return Some(Listed {
                    kind: ReaderItemKind::PartlyRead,
                    reason: outcome.reason.clone().unwrap_or_default(),
                    pages_done: pages(outcome),
                });
            }
            Some(Settled::Read) => return None,
            None if requested
                && outcome.generation == i64::from(koru_read::GENERATION)
                && outcome.status.as_deref() == Some("partial")
                && outcome.reason.is_none()
                && attempt.is_none_or(|a| verdict(a, now) != Verdict::Failed) =>
            {
                return Some(Listed {
                    kind: ReaderItemKind::PartlyRead,
                    reason: String::new(),
                    pages_done: pages(outcome),
                });
            }
            None => {}
        }
    }
    let attempt = attempt?;
    (verdict(attempt, now) == Verdict::Failed).then(|| Listed {
        kind: ReaderItemKind::CouldNotBeRead,
        reason: attempt.code.as_str().to_owned(),
        pages_done: None,
    })
}

#[cfg(test)]
mod tests;
