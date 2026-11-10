// SPDX-License-Identifier: MPL-2.0

//! [`RunDocument`]: one run, read from and written into one `LoroDoc`.
//!
//! **Reading is O(1) for the header and O(window) for the timeline**, and
//! costs no bytes once the document is open: the header is a map read, a
//! window is a list read by index. **Writing is one commit per fact**, the
//! way a run does it — a step is flushed when it happens, never batched
//! with the next — so `export_updates_since` after each is exactly the delta
//! a replica imports (proposal §1.6: 205–248 bytes a step).
//!
//! **The gate is the run's own.** `adopt` reads `run.schema` and compares it
//! with [`RUN_DOCUMENT_SCHEMA_VERSION`] through the store's `Compatibility`;
//! a newer run opens read-only and every write refuses. `meta.schema_version`
//! — the node model's key, which the projection reads on every commit — is
//! never written (proposal F3), and a test asserts its absence.

use std::collections::BTreeMap;

use koru_delegate_core::{
    EconomicsSummary, ModelIdentity, ModelRef, ResultReference, RunHeader, RunRating, RunStatus,
    TimelineEntry, UsageFact, replay,
};
use koru_store_core::version::Compatibility;
use loro::{ExportMode, LoroDoc, LoroList, LoroMap, LoroValue, ValueOrContainer, VersionVector};

use crate::RUN_DOCUMENT_SCHEMA_VERSION;
use crate::codec::{
    K_ECONOMICS, K_MODELS, K_RATING, K_RESULTS, K_SCHEMA, K_STATUS, Part, ROOT_RUN, ROOT_TIMELINE,
    RunDocumentError, StoredItem, TimelineItem, from_stored, header_from_map, header_to_keys,
    to_stored,
};

type Result<T> = std::result::Result<T, RunDocumentError>;

/// A run as one Loro document. See the crate docs for the shape.
pub struct RunDocument {
    doc: LoroDoc,
    compatibility: Compatibility,
}

impl RunDocument {
    // ------------------------------------------------------------ opening

    /// Adopt a document the store forked or served. Refuses anything that
    /// is not a run; a run from a newer build adopts read-only.
    pub fn adopt(doc: LoroDoc) -> Result<Self> {
        let found = match doc.get_map(ROOT_RUN).get(K_SCHEMA) {
            Some(ValueOrContainer::Value(LoroValue::I64(version))) => version,
            _ => return Err(RunDocumentError::NotARun),
        };
        let compatibility = Compatibility::against(Some(found), RUN_DOCUMENT_SCHEMA_VERSION);
        Ok(Self { doc, compatibility })
    }

    /// Adopt the bytes `Store::snapshot` served.
    pub fn from_snapshot(bytes: &[u8]) -> Result<Self> {
        let doc = LoroDoc::new();
        doc.import(bytes)?;
        Self::adopt(doc)
    }

    /// Write a header into an empty document — the one the store handed
    /// back from `create_document`, or a fresh `LoroDoc` whose first update
    /// will be applied to it — stamping `run.schema` and nothing under
    /// `meta`. One commit; the caller exports it with
    /// [`export_updates_since`](Self::export_updates_since) from the empty
    /// version.
    pub fn initialize(doc: LoroDoc, header: &RunHeader) -> Result<Self> {
        let run = doc.get_map(ROOT_RUN);
        if run.get(K_SCHEMA).is_some() {
            return Err(RunDocumentError::AlreadyInitialized);
        }
        run.insert(K_SCHEMA, RUN_DOCUMENT_SCHEMA_VERSION)?;
        for (key, value) in header_to_keys(header)? {
            run.insert(&key, value)?;
        }
        run.insert_container(K_MODELS, LoroMap::new())?;
        doc.commit();
        Ok(Self {
            doc,
            compatibility: Compatibility::Writable,
        })
    }

    /// [`initialize`](Self::initialize) on a fresh document.
    pub fn create(header: &RunHeader) -> Result<Self> {
        Self::initialize(LoroDoc::new(), header)
    }

    // ------------------------------------------------------------ the gate

    pub fn compatibility(&self) -> Compatibility {
        self.compatibility
    }

    /// `run.schema` as stored.
    pub fn schema_version(&self) -> Option<i64> {
        match self.run().get(K_SCHEMA) {
            Some(ValueOrContainer::Value(LoroValue::I64(version))) => Some(version),
            _ => None,
        }
    }

    fn writable(&self) -> Result<()> {
        match self.compatibility {
            Compatibility::Writable => Ok(()),
            Compatibility::TooNew { found, understood } => {
                Err(RunDocumentError::ReadOnly { found, understood })
            }
        }
    }

    /// The document itself, for the store seams that take one.
    pub fn doc(&self) -> &LoroDoc {
        &self.doc
    }

    // ------------------------------------------------------------ reading

    fn run(&self) -> LoroMap {
        self.doc.get_map(ROOT_RUN)
    }

    fn timeline(&self) -> LoroList {
        self.doc.get_list(ROOT_TIMELINE)
    }

    /// Status, device, agent, results, economics summary, rating: the
    /// header as a device reads it. O(1) map reads.
    pub fn header(&self) -> Result<RunHeader> {
        header_from_map(&self.run().get_deep_value())
    }

    /// The retained model metadata, by the reference a usage fact names
    /// (§17.3). Empty for a run written before any model was seen.
    pub fn models(&self) -> Result<BTreeMap<ModelRef, ModelIdentity>> {
        let Some(ValueOrContainer::Container(loro::Container::Map(models))) =
            self.run().get(K_MODELS)
        else {
            return Ok(BTreeMap::new());
        };
        let LoroValue::Map(entries) = models.get_deep_value() else {
            return Err(RunDocumentError::Malformed { part: Part::Models });
        };
        entries
            .values()
            .map(|value| {
                let identity: ModelIdentity = from_stored(value.clone(), Part::Models)?;
                Ok((identity.id, identity))
            })
            .collect()
    }

    /// Steps and usage facts together, as the list holds them.
    pub fn timeline_len(&self) -> usize {
        self.timeline().len()
    }

    /// A page of the activity by index, newest first: `newest_first_from`
    /// is how many of the newest entries to skip, `len` how many to return.
    /// O(window), and no bytes are read — the list is in memory once the
    /// document is open.
    pub fn timeline_window(&self, newest_first_from: usize, len: usize) -> Vec<TimelineItem> {
        let list = self.timeline();
        let end = list.len().saturating_sub(newest_first_from);
        let start = end.saturating_sub(len);
        (start..end)
            .rev()
            .filter_map(|index| list.get(index))
            .map(|item| match item {
                ValueOrContainer::Value(value) => TimelineItem::decode(value),
                ValueOrContainer::Container(_) => TimelineItem::Unrecognized,
            })
            .collect()
    }

    /// Every item, oldest first — what a fold reads.
    pub fn timeline_all(&self) -> Vec<TimelineItem> {
        self.timeline()
            .to_vec()
            .into_iter()
            .map(TimelineItem::decode)
            .collect()
    }

    /// The economics fold (§17.2, proposal §6): the usage facts replayed to
    /// the highest revision per `(attempt, turn)`, projected with the steps.
    /// Computed from the entries; the header's `economics` is a cache of
    /// this that the executing device refreshes. `now_ms` closes an interval
    /// a still-running run has open.
    pub fn economics(&self, now_ms: i64) -> EconomicsSummary {
        let mut steps: Vec<TimelineEntry> = Vec::new();
        let mut facts: Vec<UsageFact> = Vec::new();
        for item in self.timeline_all() {
            match item {
                TimelineItem::Step(entry) => steps.push(entry),
                TimelineItem::Usage(fact) => facts.push(fact),
                TimelineItem::Unrecognized => {}
            }
        }
        EconomicsSummary::project(&replay(&facts), &steps, now_ms)
    }

    // ------------------------------------------------------------ writing

    fn commit_value(&self, key: &str, value: LoroValue) -> Result<()> {
        self.writable()?;
        self.run().insert(key, value)?;
        self.doc.commit();
        Ok(())
    }

    /// Append one step (D45). One commit.
    pub fn append_step(&self, entry: &TimelineEntry) -> Result<()> {
        self.append(&StoredItem::Step(entry.clone()))
    }

    /// Commit one typed economics fact in the run's own storage — before it
    /// is published (§17.1). One commit; a retry that appends the same fact
    /// again is folded away by [`economics`](Self::economics).
    pub fn record_usage(&self, fact: &UsageFact) -> Result<()> {
        self.append(&StoredItem::Usage(fact.clone()))
    }

    fn append(&self, item: &StoredItem) -> Result<()> {
        self.writable()?;
        self.timeline().push(to_stored(item, Part::Timeline)?)?;
        self.doc.commit();
        Ok(())
    }

    /// Retain what a provider said about a model, under the reference the
    /// usage facts name it by (§17.3). Writing the same reference again
    /// replaces what it held.
    pub fn record_model(&self, identity: &ModelIdentity) -> Result<()> {
        self.writable()?;
        let run = self.run();
        let models = match run.get(K_MODELS) {
            Some(ValueOrContainer::Container(loro::Container::Map(models))) => models,
            _ => run.insert_container(K_MODELS, LoroMap::new())?,
        };
        models.insert(
            &identity.id.to_storage(),
            to_stored(identity, Part::Models)?,
        )?;
        self.doc.commit();
        Ok(())
    }

    /// The one canonical status (§7.1).
    pub fn set_status(&self, status: &RunStatus) -> Result<()> {
        self.set_status_with_failure_reason(status, None)
    }

    /// Status and safe cause form one document update; leaving failure clears its cause.
    pub fn set_status_with_failure_reason(
        &self,
        status: &RunStatus,
        reason: Option<koru_delegate_core::RunFailureReason>,
    ) -> Result<()> {
        self.writable()?;
        let stored = to_stored(status, Part::Header)?;
        let reason = reason.filter(|_| matches!(status, RunStatus::Failed { .. }));
        let reason = reason
            .map(|reason| to_stored(&reason, Part::Header))
            .transpose()?;
        let run = self.run();
        run.insert(K_STATUS, stored)?;
        if let Some(reason) = reason {
            run.insert("failure_reason", reason)?;
        } else {
            run.delete("failure_reason")?;
        }
        self.doc.commit();
        Ok(())
    }

    /// Name a result (D27), in registration order after the ones before it.
    pub fn register_result(&self, reference: &ResultReference) -> Result<()> {
        self.writable()?;
        let mut results: Vec<ResultReference> = match self.run().get(K_RESULTS) {
            Some(ValueOrContainer::Value(value)) => from_stored(value, Part::Results)?,
            _ => Vec::new(),
        };
        results.push(reference.clone());
        self.commit_value(K_RESULTS, to_stored(&results, Part::Results)?)
    }

    /// Register a verified native result once. A repeated operation must carry
    /// precisely its original evidence; later page edits never rewrite history.
    pub fn register_native_result(
        &self,
        reference: &koru_delegate_core::NativeResultReference,
    ) -> Result<()> {
        self.writable()?;
        let mut header = self.header()?;
        if header.bot.is_none() || !reference.valid() {
            return Err(RunDocumentError::Malformed { part: Part::Header });
        }
        if let Some(existing) = header
            .native_results
            .iter()
            .find(|r| r.operation_id == reference.operation_id)
        {
            return if existing == reference {
                Ok(())
            } else {
                Err(RunDocumentError::Malformed { part: Part::Header })
            };
        }
        header.native_results.push(reference.clone());
        self.commit_value(
            "native_results",
            to_stored(&header.native_results, Part::Header)?,
        )
    }

    /// One gesture on a finished run (D44). A register: the last write on
    /// any device wins, and it converges however many write it.
    pub fn set_rating(&self, rating: &RunRating) -> Result<()> {
        self.commit_value(K_RATING, to_stored(rating, Part::Header)?)
    }

    /// Recompute the fold and cache it in the header. Returns what it wrote.
    pub fn refresh_economics(&self, now_ms: i64) -> Result<EconomicsSummary> {
        let summary = self.economics(now_ms);
        self.commit_value(K_ECONOMICS, to_stored(&summary, Part::Economics)?)?;
        Ok(summary)
    }

    // ------------------------------------------------------------ sync

    /// The version everything committed so far is at.
    pub fn version(&self) -> VersionVector {
        self.doc.oplog_vv()
    }

    /// The bytes a replica at `since` needs: the executing device's delta
    /// after each step, and what one root sends another.
    pub fn export_updates_since(&self, since: &VersionVector) -> Result<Vec<u8>> {
        Ok(self.doc.export(ExportMode::updates(since))?)
    }

    /// The whole document, for a replica that has nothing.
    pub fn export_snapshot(&self) -> Result<Vec<u8>> {
        Ok(self.doc.export(ExportMode::Snapshot)?)
    }

    /// Merge what another replica exported. Convergence is Loro's: the list
    /// is append-only, the header last-writer-wins.
    pub fn import(&self, bytes: &[u8]) -> Result<()> {
        self.doc.import(bytes)?;
        Ok(())
    }
}
