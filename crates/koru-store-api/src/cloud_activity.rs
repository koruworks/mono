// SPDX-License-Identifier: MPL-2.0

//! **What the gateway's Cloud passes did, remembered in memory only.**
//!
//! The Cloud driver already reported its progress to the log and nowhere else,
//! which meant the only way to see whether a document had reached Koru Cloud
//! was to read the gateway log file beside a Docker log. This is the same
//! facts, kept where a query can answer them.
//!
//! **Memory only.** There is no table, no migration and no file behind this: a
//! restart forgets everything here, and that is correct rather than a gap.
//! `cloud_driver`'s own header states the rule this obeys — *Progress is
//! reported back through the control host's in-memory status only; `Settled`
//! here is not coverage, and nothing is written down about a pass.* A record
//! that survived a restart would start to look like durable Cloud coverage,
//! and the one thing that may establish that is the durable receipt and
//! version evidence.
//!
//! **A map rather than a queue, keyed by the thing it is about**, which is the
//! idiom `Store::committed` uses for the same reason: a hundred passes over
//! one document are one entry naming the newest facts, which is what a reader
//! wants, since it asks about the document in front of it rather than
//! replaying a history of events. Nothing drains this — a person may ask about
//! a document an hour after its pass — so unlike `committed` it also carries a
//! hard ceiling and evicts by least-recently-touched. A workspace with fifty
//! thousand documents must not become fifty thousand entries.

use std::collections::HashMap;

use uuid::Uuid;

use koru_store_wire::internal::{CloudDocumentActivity, CloudDocumentProgress, CloudPassActivity};

/// How many documents keep a record. Well past the handful a person has open
/// while they are watching two roots agree, and small enough that the whole
/// map is a rounding error beside one open document.
pub const MAX_DOCUMENTS: usize = 512;

/// Recent progress outlives a live permit. The coordinator can visit 128
/// Workspace/Profile pairs while rotating at most 32 live permits; retain
/// their scalar progress rather than evicting a finished row mid-catalog.
pub const MAX_DATASETS: usize = 256;

/// One unit's step through a pass, as the driver saw it.
///
/// Deliberately not `koru_replication::cloud::transfer::Progress`: this crate
/// holds no replication vocabulary, and the driver already has to decide which
/// ids a `Unit` carries before a record can be keyed by them.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum UnitStep {
    Started,
    Uploaded,
    Downloaded,
    Deferred,
    Settled,
}

/// The counts a finished pass reports.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct PassCounts {
    pub settled: u32,
    pub uploaded: u32,
    pub downloaded: u32,
    pub deferred: u32,
    pub remote_hint: bool,
}

#[derive(Clone, Debug, Default)]
struct DocumentRecord {
    facts: CloudDocumentActivity,
    /// When this entry last moved. The eviction order, and nothing else —
    /// never reported, because "when we last thought about it" is not a fact
    /// about the document.
    touched_at: i64,
}

#[derive(Clone, Debug, Default)]
struct DatasetRecord {
    facts: CloudPassActivity,
    touched_at: i64,
}

/// The bounded, coalesced record itself.
///
/// Every mutator takes the moment as an argument rather than reading a clock,
/// which is the rule `koru_store_core::model::Timestamp` states for the whole
/// store: *callers that need determinism take a `Timestamp` as an argument*.
/// It is also what lets the tests below assert coalescing without sleeping.
#[derive(Debug, Default)]
pub struct CloudActivity {
    documents: HashMap<(Uuid, Uuid), DocumentRecord>,
    /// Keyed by the dataset's own id — the workspace id for a Workspace
    /// dataset, the profile id for a Profile one. A pass belongs to exactly
    /// one of the two (`cloud_driver::plan_for`), so one key answers for both.
    datasets: HashMap<Uuid, DatasetRecord>,
}

impl CloudActivity {
    pub fn new() -> Self {
        Self::default()
    }

    /// A pass has been admitted and is about to run over `dataset`.
    ///
    /// The counts are cleared here rather than at the end, so a pass that is
    /// still running does not report the previous one's totals as its own.
    pub fn pass_started(&mut self, dataset: Uuid, at: i64) {
        let record = self.dataset_mut(dataset, at);
        record.facts = CloudPassActivity {
            confirmed_documents: record.facts.confirmed_documents.take(),
            started_at: Some(at),
            ..CloudPassActivity::default()
        };
        if let Some(confirmed) = &record.facts.confirmed_documents {
            tracing::debug!(%dataset, synced = confirmed.synced, required = confirmed.required,
                complete = confirmed.complete, "cloud progress: retaining confirmed documents for recheck");
        }
    }

    pub fn document_progress(
        &mut self,
        dataset: Uuid,
        synced: u32,
        required: Option<u32>,
        at: i64,
    ) {
        let facts = &mut self.dataset_mut(dataset, at).facts;
        // A backwards step within one pass means its final proof check found
        // a changed revision. A new pass's counter starts at zero separately.
        let invalidated = synced < facts.documents_synced;
        facts.documents_synced = synced;
        if required.is_some() {
            facts.documents_required = required;
        }
        if facts.documents_changed {
            return;
        }
        if let Some(required) = facts.documents_required {
            match &mut facts.confirmed_documents {
                Some(confirmed) if confirmed.required == required && !invalidated => {
                    confirmed.synced = confirmed.synced.max(synced.min(required));
                }
                _ => {
                    facts.confirmed_documents = Some(CloudDocumentProgress {
                        required,
                        synced: synced.min(required),
                        complete: false,
                    });
                }
            }
        }
    }

    /// The existing commit stream is a conservative invalidation, including
    /// imported changes. A quiet follow-up pass confirms the final inventory.
    pub fn documents_changed(&mut self, dataset: Uuid) {
        if let Some(record) = self.datasets.get_mut(&dataset) {
            record.facts.documents_changed = true;
            record.facts.confirmed_documents = None;
        }
    }

    /// One unit moved. `document` is `Some` only for a Document unit; a
    /// Profile unit (Identity, Private, Personal) is a fact about the dataset
    /// and about no document at all.
    pub fn unit(&mut self, dataset: Uuid, document: Option<(Uuid, Uuid)>, step: UnitStep, at: i64) {
        {
            let pass = &mut self.dataset_mut(dataset, at).facts;
            match step {
                UnitStep::Started => {}
                UnitStep::Uploaded => pass.uploaded = pass.uploaded.saturating_add(1),
                UnitStep::Downloaded => pass.downloaded = pass.downloaded.saturating_add(1),
                UnitStep::Deferred => pass.deferred = pass.deferred.saturating_add(1),
                UnitStep::Settled => pass.settled = pass.settled.saturating_add(1),
            }
        }
        let Some(key) = document else {
            return;
        };
        let facts = &mut self.document_mut(key, at).facts;
        let slot = match step {
            UnitStep::Started => &mut facts.last_started_at,
            UnitStep::Uploaded => &mut facts.last_uploaded_at,
            UnitStep::Downloaded => &mut facts.last_downloaded_at,
            UnitStep::Deferred => &mut facts.last_deferred_at,
            UnitStep::Settled => &mut facts.last_settled_at,
        };
        *slot = Some(at);
    }

    /// The pass ended without a refusal.
    pub fn pass_finished(&mut self, dataset: Uuid, counts: PassCounts, at: i64) {
        let pass = &mut self.dataset_mut(dataset, at).facts;
        pass.finished_at = Some(at);
        pass.settled = counts.settled;
        pass.uploaded = counts.uploaded;
        pass.downloaded = counts.downloaded;
        pass.deferred = counts.deferred;
        pass.remote_hint = counts.remote_hint;
        pass.refusal_code = None;
        pass.refused_at = None;
        if let Some(confirmed) = &mut pass.confirmed_documents {
            confirmed.complete = !pass.documents_changed
                && !counts.remote_hint
                && counts.deferred == 0
                && pass.documents_required == Some(pass.documents_synced);
            // A remote hint can announce a different inventory even before
            // its local commit arrives. Do not retain an old lower bound.
            if counts.remote_hint {
                confirmed.synced = pass.documents_synced.min(confirmed.required);
            }
        }
    }

    /// The pass ended on a refusal. `code` is the stable lowercase token the
    /// refusal already carries, never a message.
    ///
    /// The code lands on the dataset **and** on every document this pass had
    /// started, because "waiting: sync_transfer_timeout" is the answer a
    /// person is looking for when they ask about the document in front of
    /// them, and a refusal ends the pass for all of them at once.
    pub fn pass_refused(&mut self, dataset: Uuid, code: &str, at: i64, touched: &[(Uuid, Uuid)]) {
        {
            let pass = &mut self.dataset_mut(dataset, at).facts;
            pass.finished_at = Some(at);
            pass.refusal_code = Some(code.to_owned());
            pass.refused_at = Some(at);
            if let Some(confirmed) = &mut pass.confirmed_documents {
                confirmed.complete = false;
            }
        }
        for key in touched {
            let facts = &mut self.document_mut(*key, at).facts;
            facts.last_refusal_code = Some(code.to_owned());
            facts.last_refused_at = Some(at);
        }
    }

    /// **One unit has been refused on enough consecutive passes to be called
    /// unavailable**, on a pass that otherwise finished.
    ///
    /// Separate from [`Self::pass_refused`] because the two are different
    /// facts and a reader must be able to tell them apart: that one says the
    /// pass stopped and nothing after the refusal ran, this one says the pass
    /// ran to the end and one unit in it is not syncing. `finished_at` and the
    /// counts are deliberately left alone here — they are true.
    ///
    /// The code lands on the dataset as well as on the document, because a
    /// dataset with a unit that is not syncing is a dataset in trouble, and
    /// the stage a person looks at is the dataset's.
    pub fn unit_unavailable(
        &mut self,
        dataset: Uuid,
        document: Option<(Uuid, Uuid)>,
        code: &str,
        at: i64,
    ) {
        {
            let pass = &mut self.dataset_mut(dataset, at).facts;
            pass.refusal_code = Some(code.to_owned());
            pass.refused_at = Some(at);
            if let Some(confirmed) = &mut pass.confirmed_documents {
                confirmed.complete = false;
            }
        }
        let Some(key) = document else {
            return;
        };
        let facts = &mut self.document_mut(key, at).facts;
        facts.last_refusal_code = Some(code.to_owned());
        facts.last_refused_at = Some(at);
    }

    pub fn document(&self, workspace: Uuid, document: Uuid) -> Option<CloudDocumentActivity> {
        self.documents
            .get(&(workspace, document))
            .map(|record| record.facts.clone())
    }

    pub fn dataset(&self, dataset: Uuid) -> Option<CloudPassActivity> {
        self.datasets
            .get(&dataset)
            .map(|record| record.facts.clone())
    }

    #[cfg(test)]
    fn documents_held(&self) -> usize {
        self.documents.len()
    }

    #[cfg(test)]
    fn datasets_held(&self) -> usize {
        self.datasets.len()
    }

    fn document_mut(&mut self, key: (Uuid, Uuid), at: i64) -> &mut DocumentRecord {
        if !self.documents.contains_key(&key) {
            evict(&mut self.documents, MAX_DOCUMENTS, |record| {
                record.touched_at
            });
        }
        let record = self.documents.entry(key).or_default();
        record.touched_at = at;
        record
    }

    fn dataset_mut(&mut self, key: Uuid, at: i64) -> &mut DatasetRecord {
        if !self.datasets.contains_key(&key) {
            evict(&mut self.datasets, MAX_DATASETS, |record| record.touched_at);
        }
        let record = self.datasets.entry(key).or_default();
        record.touched_at = at;
        record
    }
}

/// Make room for one more entry, dropping the least recently touched.
///
/// A loop rather than a single removal, so a ceiling that is lowered in a
/// later build settles on the first insert instead of staying over it forever.
fn evict<K, V, F>(map: &mut HashMap<K, V>, ceiling: usize, touched: F)
where
    K: Clone + std::hash::Hash + Eq,
    F: Fn(&V) -> i64,
{
    while map.len() >= ceiling {
        let Some(oldest) = map
            .iter()
            .min_by_key(|(_, value)| touched(value))
            .map(|(key, _)| key.clone())
        else {
            return;
        };
        map.remove(&oldest);
    }
}

#[cfg(test)]
mod tests;
