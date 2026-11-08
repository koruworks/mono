// SPDX-License-Identifier: MPL-2.0

//! Plan B4's six cases, against the approved `logical-usage-v1` corpus, plus
//! the rules the coverage brief states in prose and that only a test can hold.
//!
//! **The corpus decides what is live; this module never decides it again.**
//! Every case below that is about *which references exist* reads its documents
//! out of `contracts/vectors/logical-usage-v1/` and asserts what the corpus
//! already says. What the tests add is the second half — whether Cloud holds
//! them — which the corpus also supplies, in each blob's `durable` flag and
//! its `verified_plaintext_length_bytes`.

use std::collections::BTreeMap;

use koru_device::cloud::CloudId;
use koru_store_core::{DocumentId, WorkspaceId, model::BlobHash};

use super::*;
use crate::vectors;

fn workspace() -> WorkspaceId {
    WorkspaceId::new()
}

fn incarnation() -> CloudId {
    CloudId::from_bytes(*uuid::Uuid::new_v4().as_bytes())
}

/// An ordered document state. Two numbers are two states and the higher one
/// includes the lower, which is all any case here needs of a version vector.
fn state(counter: i32) -> DocumentState {
    crate::fakes::state_of(counter)
}

/// The corpus case of this name, as a sample: one `SampledDocument` per
/// document in the case's first workspace, each at its own state.
fn sample_from(case_id: &str) -> (DocumentSample, BTreeMap<String, vectors::InputBlob>) {
    let case = vectors::cases()
        .into_iter()
        .find(|case| case.id == case_id)
        .unwrap_or_else(|| panic!("{case_id} is not in the corpus"));
    let workspace = case
        .input
        .workspaces
        .first()
        .unwrap_or_else(|| panic!("{case_id} has no workspace"))
        .clone();
    let documents = workspace
        .walk_documents()
        .unwrap_or_else(|| panic!("{case_id} enumerates no documents"));
    let sample = DocumentSample::new(
        documents
            .into_iter()
            .enumerate()
            .map(|(ordinal, content)| SampledDocument {
                document: DocumentId::new(),
                state: Some(state(ordinal as i32 + 1)),
                content,
            })
            .collect(),
    );
    (sample, workspace.blobs_by_address())
}

/// Every document of the sample, covered at exactly the state it was sampled
/// at — the ordinary case, and the baseline every blob case varies from.
fn all_covered(sample: &DocumentSample) -> CoveredDocuments {
    sample
        .documents
        .iter()
        .map(|document| (document.document, document.state.clone()))
        .collect()
}

fn request(workspace: WorkspaceId, documents: CoveredDocuments) -> CoverageRequest {
    CoverageRequest {
        workspace,
        incarnation: incarnation(),
        documents,
    }
}

/// What the corpus says Cloud holds, turned into presences. A blob the corpus
/// marks durable with a verified length is durable; anything else is missing.
fn presence_from_corpus(
    blobs: &BTreeMap<String, vectors::InputBlob>,
) -> BTreeMap<String, BlobPresence> {
    blobs
        .iter()
        .map(|(address, blob)| {
            let presence = match (blob.durable, blob.verified_plaintext_length_bytes) {
                (true, Some(size)) => BlobPresence::Durable { size },
                _ => BlobPresence::Missing,
            };
            (address.clone(), presence)
        })
        .collect()
}

fn durable(sample: &DocumentSample, size: u64) -> BTreeMap<String, BlobPresence> {
    sample
        .walk()
        .blobs
        .into_iter()
        .map(|blob| {
            (
                blob.hash.as_str().to_owned(),
                BlobPresence::Durable { size },
            )
        })
        .collect()
}

// ----------------------------------------------------- Plan B4's six cases

/// **Missing.** Every document is synced and one picture is not, and the
/// Workspace is not current — with the two numbers saying which of them is
/// the problem.
#[test]
fn a_missing_picture_is_not_a_missing_document() {
    let (sample, _) = sample_from("09-blob-referenced-once");
    let request = request(workspace(), all_covered(&sample));
    let report = compute(&request, &sample, &BTreeMap::new());
    assert!(!report.is_current(), "{report:?}");
    assert_eq!(report.documents_missing, 0, "a document was blamed");
    assert_eq!(report.blobs_referenced, 1);
    assert_eq!(report.blobs_missing, 1);
    assert_eq!(report.blobs_durable, 0);
}

/// **Stale.** The pass covered an older state than the one this sample read,
/// so the newer document is not covered — even though every picture the newer
/// state names is durable.
///
/// This is the brief's first consequence: *a blob uploaded after the sampled document
/// changed does not cover the newer document*. Coverage never runs ahead of
/// what it sampled.
#[test]
fn a_document_ahead_of_what_cloud_holds_is_not_covered() {
    let (sample, _) = sample_from("09-blob-referenced-once");
    let stale: CoveredDocuments = sample
        .documents
        .iter()
        .map(|document| (document.document, Some(state(0))))
        .collect();
    let report = compute(
        &request(workspace(), stale),
        &sample,
        &durable(&sample, 4_096),
    );
    assert_eq!(report.blobs_missing, 0, "the picture is there");
    assert_eq!(report.documents_missing, 1, "the newer document is not");
    assert!(!report.is_current());
}

/// **A read model that has not caught up is not coverage either.** The
/// references it holds are an older set than the state the pass carried, so
/// counting them complete would be claiming completeness over references
/// nothing has looked at.
#[test]
fn a_sample_behind_what_the_pass_carried_is_not_covered() {
    let (sample, _) = sample_from("09-blob-referenced-once");
    let ahead: CoveredDocuments = sample
        .documents
        .iter()
        .map(|document| (document.document, Some(state(99))))
        .collect();
    let report = compute(
        &request(workspace(), ahead),
        &sample,
        &durable(&sample, 4_096),
    );
    assert_eq!(report.documents_missing, 1, "{report:?}");
    assert_eq!(report.blobs_missing, 0, "the blobs were blamed");
    assert!(!report.is_current());
}

/// **Unreferenced.** A durable mapping for an address nothing names improves
/// nothing, and is not counted as anything at all.
#[test]
fn a_mapping_for_an_unreferenced_hash_does_not_improve_coverage() {
    let (sample, _) = sample_from("09-blob-referenced-once");
    let request = request(workspace(), all_covered(&sample));
    let mut presence = BTreeMap::new();
    let stranger = BlobHash::parse(&vectors::address_of("nothing-names-this")).expect("an address");
    presence.insert(
        stranger.as_str().to_owned(),
        BlobPresence::Durable { size: 1_000_000 },
    );
    let report = compute(&request, &sample, &presence);
    assert_eq!(report.blobs_referenced, 1, "the stranger was counted");
    assert_eq!(report.blobs_durable, 0, "the stranger improved coverage");
    assert_eq!(report.blobs_missing, 1);
    assert!(!report.is_current());
}

/// **Trashed.** A picture referenced only from the Trash is still owed —
/// corpus case 12 says so — so a Workspace whose only reference is binned is
/// not current until that picture is durable.
#[test]
fn a_reference_only_from_the_trash_is_still_required() {
    let (sample, _) = sample_from("12-blob-referenced-only-from-trash");
    let request = request(workspace(), all_covered(&sample));
    assert_eq!(
        sample.walk().blobs.len(),
        1,
        "the corpus case lost its binned reference"
    );
    let missing = compute(&request, &sample, &BTreeMap::new());
    assert_eq!(missing.blobs_missing, 1);
    assert!(!missing.is_current());

    let held = compute(&request, &sample, &durable(&sample, 2_048));
    assert!(held.is_current(), "{held:?}");
}

/// **Duplicate.** One picture named twice is one object, so one durable
/// mapping covers both references — corpus case 10.
#[test]
fn one_picture_named_twice_needs_one_mapping() {
    let (sample, _) = sample_from("10-blob-referenced-twice-in-one-workspace");
    let walk = sample.walk();
    assert_eq!(
        walk.blobs.len(),
        1,
        "the corpus case lost its deduplication"
    );
    assert_eq!(walk.blobs[0].references, 2);
    let report = compute(
        &request(workspace(), all_covered(&sample)),
        &sample,
        &durable(&sample, 512),
    );
    assert_eq!(report.blobs_referenced, 1);
    assert_eq!(report.blobs_durable, 1);
    assert!(report.is_current());
}

/// **Concurrently changed.** One document moved on while its blobs were being
/// carried. The picture is durable, the document is not covered, and the two
/// numbers say exactly that rather than one of them standing in for both.
#[test]
fn a_document_that_changed_beside_its_blobs_is_reported_on_its_own() {
    let (sample, _) = sample_from("10-blob-referenced-twice-in-one-workspace");
    let mut covered = all_covered(&sample);
    let moved = sample.documents[0].document;
    covered.insert(moved, Some(state(0)));
    let report = compute(
        &request(workspace(), covered),
        &sample,
        &durable(&sample, 512),
    );
    assert_eq!(report.blobs_missing, 0);
    assert_eq!(report.blobs_durable, 1);
    assert_eq!(report.documents_missing, 1);
    assert!(!report.is_current());
}

// ---------------------------------------------------- the rest of the rules

/// **Not yet durable.** Corpus case 19 is a blob whose upload never finalized:
/// Cloud holds no bytes and there is no verified length. It is not coverage.
#[test]
fn the_corpus_case_for_a_blob_that_is_not_durable_is_not_covered() {
    let (sample, blobs) = sample_from("19-blob-not-yet-durable");
    let report = compute(
        &request(workspace(), all_covered(&sample)),
        &sample,
        &presence_from_corpus(&blobs),
    );
    assert!(report.blobs_referenced > 0, "the case references nothing");
    assert_eq!(report.blobs_durable, 0);
    assert_eq!(report.blobs_missing, report.blobs_referenced);
    assert!(!report.is_current());
}

/// Zero cannot establish coverage for a nonempty content address.
#[test]
fn a_durable_mapping_without_a_verified_size_is_not_coverage() {
    let (sample, _) = sample_from("09-blob-referenced-once");
    let report = compute(
        &request(workspace(), all_covered(&sample)),
        &sample,
        &durable(&sample, 0),
    );
    assert_eq!(report.blobs_durable, 0);
    assert_eq!(report.blobs_missing, 1);
    assert!(!report.is_current());
}

#[test]
fn a_verified_empty_file_counts_toward_coverage() {
    let sample = DocumentSample::new(vec![SampledDocument {
        document: DocumentId::new(),
        state: Some(state(1)),
        content: WalkDocument {
            durable: true,
            trashed: false,
            nodes: vec![crate::referenced::WalkNode {
                blob: Some(koru_cloud_blob::EMPTY_BLOB_HASH.to_owned()),
                mime: Some("application/octet-stream".into()),
            }],
        },
    }]);
    let report = compute(
        &request(workspace(), all_covered(&sample)),
        &sample,
        &durable(&sample, 0),
    );
    assert_eq!(report.blobs_durable, 1);
    assert!(report.is_current());
}

/// A picture halfway up is not missing and is not durable. Two different
/// sentences to a person, and one of them is *wait*.
#[test]
fn a_session_in_flight_is_its_own_number() {
    let (sample, _) = sample_from("09-blob-referenced-once");
    let hash = sample.walk().blobs[0].hash.clone();
    let presence = BTreeMap::from([(hash.as_str().to_owned(), BlobPresence::Uploading)]);
    let report = compute(
        &request(workspace(), all_covered(&sample)),
        &sample,
        &presence,
    );
    assert_eq!(report.blobs_uploading, 1);
    assert_eq!(report.blobs_missing, 0);
    assert!(!report.is_current(), "an upload in flight read as complete");
}

/// A document the pass never carried is missing document state, whatever its
/// pictures are doing.
#[test]
fn a_document_the_pass_did_not_carry_is_missing() {
    let (sample, _) = sample_from("09-blob-referenced-once");
    let uncarried: CoveredDocuments = sample
        .documents
        .iter()
        .map(|document| (document.document, None))
        .collect();
    let report = compute(
        &request(workspace(), uncarried),
        &sample,
        &durable(&sample, 4_096),
    );
    assert_eq!(report.documents_missing, 1);
    assert_eq!(report.blobs_missing, 0);
    assert!(!report.is_current());
}

/// **An unreadable reference is not silently excluded.** A coverage number
/// that dropped it would claim a completeness it has not got — corpus case 14
/// is the shape.
#[test]
fn an_unreadable_reference_keeps_a_workspace_from_being_current() {
    let sample = DocumentSample::new(vec![SampledDocument {
        document: DocumentId::new(),
        state: Some(state(1)),
        content: crate::referenced::WalkDocument {
            durable: true,
            trashed: false,
            nodes: vec![crate::referenced::WalkNode {
                blob: Some("not-an-address".to_owned()),
                mime: None,
            }],
        },
    }]);
    let report = compute(
        &request(workspace(), all_covered(&sample)),
        &sample,
        &BTreeMap::new(),
    );
    assert_eq!(report.blobs_referenced, 0);
    assert_eq!(report.unreadable_references, 1);
    assert!(!report.is_current());
}

/// **A Workspace with no documents claims nothing.** Coverage that was never
/// given a document result is not a Workspace that is complete — corpus case
/// 01 has no documents and no pictures, and *complete* is a claim about a
/// check that ran.
#[test]
fn coverage_with_no_required_documents_claims_nothing() {
    let sample = DocumentSample::default();
    let report = compute(
        &request(workspace(), CoveredDocuments::new()),
        &sample,
        &BTreeMap::new(),
    );
    assert_eq!(report.documents_required, 0);
    assert!(!report.is_current());
}

// ------------------------------------------------------ step 4, the boundary

/// The sample moving between the read and the boundary supersedes the report.
#[test]
fn a_sample_that_moved_supersedes_the_report() {
    let (sample, _) = sample_from("09-blob-referenced-once");
    let request = request(workspace(), all_covered(&sample));
    let report = compute(&request, &sample, &durable(&sample, 4_096));
    assert!(report.is_current(), "the baseline was not complete");

    let moved = DocumentSample::new(
        sample
            .documents
            .iter()
            .map(|document| SampledDocument {
                state: Some(state(7)),
                ..document.clone()
            })
            .collect(),
    );
    let settled = report.settle(&moved, Some(request.incarnation));
    assert_eq!(settled.state, CoverageState::Superseded);
    assert!(!settled.is_current());
}

/// The authorization moving supersedes it too — a Workspace dropped and
/// re-provisioned is a different dataset, and a mapping under the old
/// incarnation says nothing about the new one.
#[test]
fn a_replaced_incarnation_supersedes_the_report() {
    let (sample, _) = sample_from("09-blob-referenced-once");
    let request = request(workspace(), all_covered(&sample));
    let report = compute(&request, &sample, &durable(&sample, 4_096));

    assert_eq!(
        report.clone().settle(&sample, Some(incarnation())).state,
        CoverageState::Superseded,
        "a different incarnation committed"
    );
    assert_eq!(
        report.clone().settle(&sample, None).state,
        CoverageState::Superseded,
        "a revoked authorization committed"
    );
    assert_eq!(
        report.settle(&sample, Some(request.incarnation)).state,
        CoverageState::Complete,
    );
}

/// The digest is over ids and states, so a sample that did not move is the
/// same sample however its rows came back.
#[test]
fn a_sample_digest_ignores_order_and_notices_a_state() {
    let one = DocumentId::new();
    let two = DocumentId::new();
    let content = crate::referenced::WalkDocument {
        durable: true,
        trashed: false,
        nodes: Vec::new(),
    };
    let a = SampledDocument {
        document: one,
        state: Some(state(1)),
        content: content.clone(),
    };
    let b = SampledDocument {
        document: two,
        state: Some(state(2)),
        content: content.clone(),
    };
    assert_eq!(
        DocumentSample::new(vec![a.clone(), b.clone()]).digest(),
        DocumentSample::new(vec![b.clone(), a.clone()]).digest(),
    );
    assert_ne!(
        DocumentSample::new(vec![a.clone(), b.clone()]).digest(),
        DocumentSample::new(vec![
            a,
            SampledDocument {
                state: Some(state(3)),
                ..b
            }
        ])
        .digest(),
    );
}

/// A state that will not decode includes nothing. Answering *yes* on a parse
/// failure is how a claim of completeness gets made out of a broken read.
#[test]
fn an_undecodable_state_includes_nothing() {
    let broken = DocumentState::from_bytes(vec![0xff, 0xff, 0xff, 0xff]);
    assert!(!broken.includes(&state(1)));
    assert!(
        broken.includes(&DocumentState::default()),
        "the empty state is included by everything"
    );
}

/// **A listed document the read model has never projected is not covered.**
///
/// Its references are unknown, so a report that counted it as covered would be
/// claiming completeness over a document nothing has read — which is exactly
/// how a Workspace gets a green tick while one of its pictures has never been
/// looked for.
#[test]
fn a_document_that_has_never_been_projected_is_not_covered() {
    let unread = DocumentId::new();
    let sample = DocumentSample::new(vec![SampledDocument {
        document: unread,
        state: None,
        content: crate::referenced::WalkDocument {
            durable: true,
            trashed: false,
            nodes: Vec::new(),
        },
    }]);
    let report = compute(
        &request(
            workspace(),
            CoveredDocuments::from([(unread, Some(state(9)))]),
        ),
        &sample,
        &BTreeMap::new(),
    );
    assert_eq!(report.documents_missing, 1, "{report:?}");
    assert_eq!(report.blobs_referenced, 0);
    assert!(!report.is_current());
}
