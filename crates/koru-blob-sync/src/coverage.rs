// SPDX-License-Identifier: MPL-2.0

//! Cloud coverage, extended from the documents to the pictures they name.
//!
//! Before this module, "Cloud is current" meant *the documents synced*. It now
//! means *the documents **and** the pictures they reference synced*, and the
//! two are counted separately — so a missing picture never reads as a missing
//! document, and a person is never told a Workspace is complete while bytes
//! are still moving.
//!
//! ## The four steps, in order, and why each one is a step
//!
//! 1. **Sample the exact per-document states the document check already
//!    used.** Not a fresh read. A second read is a second state, and a
//!    coverage claim over state B built from a document check over state A is
//!    a claim about a Workspace that never existed.
//! 2. **Extract the unique live blob references from those sampled states**,
//!    through [`crate::referenced`] — the one canonical walk. There is no
//!    second definition of "referenced" here and there must never be one:
//!    the failure that costs a person a picture is three walkers with three
//!    ideas of *live*.
//! 3. **Require a durable mapping with the verified size** for every
//!    reference, under the **same Workspace incarnation**. A session that is
//!    still uploading is not coverage; a mapping under a previous incarnation
//!    is a mapping for a dataset this one replaced.
//! 4. **Commit only if the authorization and the sample still match** at the
//!    transaction boundary. Anything else is a report about a Workspace as it
//!    was some milliseconds ago, published as though it were now.
//!
//! ## Three consequences the plan states, and which this module has to obey
//!
//! - **A blob uploaded *after* the sampled document changed does not cover the
//!   newer document.** Coverage never runs ahead of what it sampled: the
//!   comparison is *does the state Cloud holds include the state we sampled*,
//!   so a document that moved on after the pass is `missing` until the next
//!   sample establishes the relationship. That is a delay of one pass, and the
//!   alternative is a green tick over a picture nobody has uploaded yet.
//! - **A mapping for an unreferenced hash does not improve coverage.** It
//!   cannot, by construction: the only addresses looked up are the ones the
//!   walk returned. [`CoverageReport`] never counts a presence it was not
//!   asked about.
//! - **Missing document state and missing blob count are reported
//!   separately.** One number cannot say which of two very different things
//!   is wrong, and "your Workspace is not current" with no second sentence is
//!   what makes a person restart an application that was working.
//!
//! ## What is deliberately not here
//!
//! Quota and entitlement. There is no quota port in this tree and D255 says
//! that is deliberate; a coverage report that carried a byte allowance would
//! be the first half of one.

use std::collections::BTreeMap;

use koru_device::cloud::CloudId;
use koru_store_core::{DocumentId, WorkspaceId};

use crate::referenced::{Walk, WalkDocument, referenced_blobs};

/// One document's exact state, as the sample read it.
///
/// **Encoded version-vector bytes, compared as version vectors.** Clocks
/// disagree, drift and go backwards; a version vector says what a replica has
/// actually seen, which is the only comparison `PRINCIPLES.md` permits. The
/// bytes are opaque to every caller: nothing outside this type takes them
/// apart, and nothing constructs one from a timestamp or a counter.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct DocumentState(Vec<u8>);

impl DocumentState {
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// The empty state — a document this device holds nothing of. It is
    /// included by everything, which is the honest answer: a replica that has
    /// seen nothing is behind every replica that has seen something.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Does this state **include** `other`?
    ///
    /// **Inclusion, not equality** (MS1: *"a destination that has moved ahead,
    /// or that has concurrent edits of its own, is covered and must not be
    /// failed"*). A state that will not decode includes nothing — a source
    /// this build cannot read is not evidence of coverage, and answering
    /// *yes* on an undecodable vector is how a claim of completeness is made
    /// out of a parse failure.
    pub fn includes(&self, other: &Self) -> bool {
        if other.is_empty() {
            return true;
        }
        let (Ok(here), Ok(there)) = (
            loro::VersionVector::decode(&self.0),
            loro::VersionVector::decode(&other.0),
        ) else {
            return false;
        };
        here.includes_vv(&there)
    }
}

/// One document as the sample holds it: which document, the exact state that
/// was read, and the content drawn **from that state**.
///
/// The two halves travel together on purpose. A `WalkDocument` with no state
/// beside it is a set of references nobody can say *when* — and "when" is the
/// whole of what makes coverage a guarantee rather than a coincidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SampledDocument {
    pub document: DocumentId,
    /// **`None` is a real state, and it is not zero.** It means the read model
    /// has not projected this document yet, so the references it holds are
    /// unknown — and a document whose references are unknown can never be
    /// covered, however complete everything around it looks. Reading `None` as
    /// "no pictures" is how a Workspace gets reported complete while one of its
    /// documents has never been read.
    pub state: Option<DocumentState>,
    pub content: WalkDocument,
}

/// One read of a Workspace's durable state, taken once.
///
/// Not a cross-document atomic cut, and no caller may treat it as one: MS1
/// says entries may be captured at different instants and that the workspace
/// CRDT has never promised otherwise. What this *is* is a completeness report
/// over states that already exist, each one identifiable, so the report can be
/// refused when any of them moves underneath it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocumentSample {
    pub documents: Vec<SampledDocument>,
}

impl DocumentSample {
    pub fn new(documents: Vec<SampledDocument>) -> Self {
        Self { documents }
    }

    /// **Step 2**, and the only way this module reaches a blob address.
    pub fn walk(&self) -> Walk {
        let documents: Vec<WalkDocument> = self
            .documents
            .iter()
            .map(|document| document.content.clone())
            .collect();
        referenced_blobs(&documents)
    }

    /// The fingerprint step 4 compares.
    ///
    /// Over `(document, state)` pairs in address order, so two reads of an
    /// unchanged Workspace agree however the rows came back, and any document
    /// moving changes it. Content-free: ids and version bytes, never a title,
    /// a filename or a word somebody wrote.
    pub fn digest(&self) -> SampleDigest {
        let mut ordered: Vec<(&DocumentId, &Option<DocumentState>)> = self
            .documents
            .iter()
            .map(|document| (&document.document, &document.state))
            .collect();
        ordered.sort();
        let mut hasher = blake3::Hasher::new();
        for (document, state) in ordered {
            hasher.update(document.to_storage().as_bytes());
            // A discriminant, so an unprojected document and one projected at
            // the empty state are two different samples rather than one.
            match state {
                Some(state) => {
                    hasher.update(&[1]);
                    hasher.update(state.as_bytes());
                }
                None => {
                    hasher.update(&[0]);
                }
            }
            hasher.update(&[0]);
        }
        SampleDigest(*hasher.finalize().as_bytes())
    }
}

/// A sample's identity. Comparable, content-free, and cheap to take twice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SampleDigest([u8; 32]);

impl SampleDigest {
    /// Unpadded lowercase hex, for a diagnostic a person reads. Sixteen
    /// characters: enough to tell two samples apart in a log, and never
    /// something a caller is tempted to treat as authority.
    pub fn short(&self) -> String {
        self.0
            .iter()
            .take(8)
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

/// What Cloud holds for one referenced address, under the sampled
/// incarnation.
///
/// **A session in flight is its own answer.** Folding `Uploading` into
/// `Missing` would make a picture that is 90% of the way there
/// indistinguishable from one nothing has started, which is the difference
/// between "wait" and "something is wrong".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlobPresence {
    /// A durable mapping with a verified size, under this incarnation.
    Durable { size: u64 },
    /// A live upload session. Not coverage.
    Uploading,
    /// No mapping and no session.
    Missing,
}

impl BlobPresence {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Durable { .. } => "durable",
            Self::Uploading => "uploading",
            Self::Missing => "missing",
        }
    }
}

/// The document half of the request: what the Cloud pass established.
///
/// One entry per document the pass was required to carry. The value is the
/// **exact local state the pass carried to Cloud** — so Cloud holds that
/// document at or ahead of it — or `None` for a document the pass did not
/// carry at all: deferred, refused, or never reached.
///
/// **MS1's *a destination that is ahead passes* lives in the pass, not here.**
/// Cloud having moved on past this state, because another device wrote, is
/// covered and must not be failed — and it is the pass that establishes that,
/// by settling the unit. What this module compares is two facts about *this*
/// device: the state the pass carried, and the state the references were read
/// from.
pub type CoveredDocuments = BTreeMap<DocumentId, Option<DocumentState>>;

/// Everything one coverage computation is asked.
#[derive(Clone, Debug)]
pub struct CoverageRequest {
    pub workspace: WorkspaceId,
    /// The Workspace incarnation the document pass ran under. Step 3's *same
    /// incarnation*, and step 4 re-reads it.
    pub incarnation: CloudId,
    pub documents: CoveredDocuments,
}

/// Whether a computed report may be published.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoverageState {
    /// Every required document state is covered and every reference it names
    /// is durable. The one state that claims anything.
    Complete,
    /// Something is still owed, and the counts say which.
    Incomplete,
    /// The authorization or the sample moved between the read and the
    /// boundary. **Nothing is claimed**: this report describes a Workspace
    /// that has already stopped being the one it describes.
    Superseded,
}

impl CoverageState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Incomplete => "incomplete",
            Self::Superseded => "superseded",
        }
    }
}

/// What coverage found. Counts, ids and one digest; never a title, a filename
/// or a byte.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoverageReport {
    pub workspace: WorkspaceId,
    pub incarnation: CloudId,
    /// Documents the pass was required to carry.
    pub documents_required: u32,
    /// **Of those, the ones whose sampled state Cloud does not hold.** Its own
    /// number, beside the blob ones and never added to them.
    pub documents_missing: u32,
    pub blobs_referenced: u32,
    pub blobs_durable: u32,
    pub blobs_uploading: u32,
    /// **Referenced, and Cloud has nothing.** The number a person reads as
    /// *pictures still to go*.
    pub blobs_missing: u32,
    /// References the walk could not turn into an address. Counted here for
    /// the same reason the walk counts them: a coverage number that hid them
    /// would claim a completeness it has not got.
    pub unreadable_references: u32,
    pub state: CoverageState,
    pub sample: SampleDigest,
}

impl CoverageReport {
    /// The one sentence a status surface is allowed to derive from this.
    pub fn is_current(&self) -> bool {
        matches!(self.state, CoverageState::Complete)
    }

    /// Everything still owed, documents and blobs together — for a caller that
    /// wants one number *after* it has shown the two.
    pub fn outstanding(&self) -> u32 {
        self.documents_missing
            .saturating_add(self.blobs_missing)
            .saturating_add(self.blobs_uploading)
            .saturating_add(self.unreadable_references)
    }
}

/// **Steps 2 and 3**, over one sample and one presence map.
///
/// Pure: it takes the sample, the request and what Cloud answered, and returns
/// the report. Step 4 is [`CoverageReport::settle`], which the caller applies
/// once it has re-read the boundary.
///
/// `presence` is consulted only for addresses the walk returned. An entry for
/// anything else is ignored rather than counted, which is the second
/// consequence stated as code: a mapping for an unreferenced hash cannot
/// improve coverage.
pub fn compute(
    request: &CoverageRequest,
    sample: &DocumentSample,
    presence: &BTreeMap<String, BlobPresence>,
) -> CoverageReport {
    let walk = sample.walk();
    let mut durable = 0u32;
    let mut uploading = 0u32;
    let mut missing = 0u32;
    for blob in &walk.blobs {
        match presence.get(blob.hash.as_str()) {
            Some(BlobPresence::Durable { size })
                if koru_cloud_blob::valid_blob_size(blob.hash.as_str(), *size) =>
            {
                durable += 1
            }
            // Zero proves coverage only at the empty file's content address.
            Some(BlobPresence::Durable { .. }) | None => missing += 1,
            Some(BlobPresence::Uploading) => uploading += 1,
            Some(BlobPresence::Missing) => missing += 1,
        }
    }
    let documents_missing = documents_missing(request, sample);
    let complete = documents_missing == 0
        && missing == 0
        && uploading == 0
        && walk.unreadable == 0
        && !request.documents.is_empty();
    CoverageReport {
        workspace: request.workspace,
        incarnation: request.incarnation,
        documents_required: request.documents.len() as u32,
        documents_missing,
        blobs_referenced: walk.blobs.len() as u32,
        blobs_durable: durable,
        blobs_uploading: uploading,
        blobs_missing: missing,
        unreadable_references: walk.unreadable,
        state: if complete {
            CoverageState::Complete
        } else {
            CoverageState::Incomplete
        },
        sample: sample.digest(),
    }
}

/// How many required documents Cloud does not hold the sampled state of.
///
/// Three ways to be missing, and they are one sentence to a person — *Cloud
/// does not have this document as it is now*:
///
/// - **the pass did not carry it at all** — deferred, refused, never reached;
/// - **the read model has never projected it**, so the references it holds are
///   unknown and no claim can be made about them;
/// - **the state it was carried at and the state its references were read from
///   are not the same state.** Either direction is a gap and both are real: a
///   sample *ahead* of the pass is a document that moved after it was carried,
///   and Cloud holds the older one — the brief's first consequence, that a blob
///   uploaded after the sampled document changed does not cover the newer
///   document. A sample *behind* the pass is a read model that has not caught
///   up, and its references are an older set than what Cloud now holds, so
///   claiming them complete would be claiming completeness over references
///   nothing has looked at.
///
/// Both are answered the same way and neither is fatal: the next sample
/// establishes the relationship, which is what "coverage never runs ahead of
/// what it sampled" costs.
fn documents_missing(request: &CoverageRequest, sample: &DocumentSample) -> u32 {
    verdicts(request, sample)
        .filter(|(_, verdict)| matches!(verdict, Verdict::Missing))
        .count() as u32
}

/// **Which documents Cloud holds, and at which state** — the per-document
/// answer [`documents_missing`] counts the other side of.
///
/// Kept by the engine beside the presence map, so a surface explaining one
/// document can say *synced* without a second definition of it. The state is
/// the one the sample read and the pass carried (they are the same state, or
/// the document would be missing): a later read of the document at that state
/// is still held, and a read at any other state is a document that moved
/// after Cloud got it.
pub(crate) fn held_documents(
    request: &CoverageRequest,
    sample: &DocumentSample,
) -> BTreeMap<DocumentId, DocumentState> {
    verdicts(request, sample)
        .filter_map(|(document, verdict)| match verdict {
            Verdict::Held(state) => Some((document, state)),
            Verdict::Missing | Verdict::Gone => None,
        })
        .collect()
}

/// One required document, judged.
enum Verdict {
    /// Cloud holds the state the sample read.
    Held(DocumentState),
    /// One of the three ways [`documents_missing`] names.
    Missing,
    /// Required, and permanently removed before the sample read it.
    Gone,
}

fn verdicts<'a>(
    request: &'a CoverageRequest,
    sample: &'a DocumentSample,
) -> impl Iterator<Item = (DocumentId, Verdict)> + 'a {
    let sampled: BTreeMap<&DocumentId, &Option<DocumentState>> = sample
        .documents
        .iter()
        .map(|document| (&document.document, &document.state))
        .collect();
    request.documents.iter().map(move |(document, covered)| {
        let verdict = match covered {
            // The pass did not carry it at all.
            None => Verdict::Missing,
            Some(covered) => match sampled.get(document) {
                // Required, and the sample does not hold it: the listing no
                // longer names it, so it has been permanently removed between
                // the pass and this read. Not counted against coverage — it is
                // gone — and the boundary check below refuses the report
                // anyway, because a sample that lost a document has moved.
                None => Verdict::Gone,
                // Listed and never projected: its references are unknown.
                Some(None) => Verdict::Missing,
                Some(Some(state)) if covered.includes(state) && state.includes(covered) => {
                    Verdict::Held(state.clone())
                }
                Some(Some(_)) => Verdict::Missing,
            },
        };
        (*document, verdict)
    })
}

impl CoverageReport {
    /// **Step 4.** Answer whether this report may be committed.
    ///
    /// `again` is a second sample taken at the boundary and `authorization` is
    /// whether the exact incarnation that admitted the pass is still the live
    /// one. Either moving supersedes the report: a coverage claim is a
    /// statement about *now*, and both of these are the ways *now* stops being
    /// what was read.
    ///
    /// The report is returned either way, with its state changed, rather than
    /// being thrown away — a caller that cannot see it was superseded is a
    /// caller that reports nothing and says nothing about why.
    #[must_use]
    pub fn settle(mut self, again: &DocumentSample, authorization: Option<CloudId>) -> Self {
        if authorization != Some(self.incarnation) || again.digest() != self.sample {
            self.state = CoverageState::Superseded;
        }
        self
    }
}

#[cfg(test)]
mod tests;
