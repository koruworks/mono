// SPDX-License-Identifier: MPL-2.0

//! The record coalesces, and it stays bounded.

use super::*;

#[test]
fn document_inventory_and_progress_are_visible_before_pass_finishes() {
    let mut activity = CloudActivity::new();
    let dataset = Uuid::from_u128(12);
    activity.pass_started(dataset, 1);
    activity.document_progress(dataset, 1, Some(200), 2);
    activity.document_progress(dataset, 100, None, 3);
    let pass = activity.dataset(dataset).unwrap();
    assert_eq!(pass.documents_required, Some(200));
    assert_eq!(pass.documents_synced, 100);
    assert_eq!(pass.finished_at, None);
    activity.documents_changed(dataset);
    activity.pass_finished(dataset, PassCounts::default(), 3);
    assert!(
        activity.dataset(dataset).unwrap().documents_changed,
        "finishing cannot erase an edit that invalidated the sample"
    );
    activity.pass_started(dataset, 4);
    let pass = activity.dataset(dataset).unwrap();
    assert_eq!(pass.documents_required, None);
    assert_eq!(pass.documents_synced, 0);
    assert!(!pass.documents_changed);
}

fn id(byte: u8) -> Uuid {
    Uuid::from_bytes([byte; 16])
}

#[test]
fn a_recheck_retains_confirmed_progress_without_reusing_pass_counters() {
    let mut activity = CloudActivity::new();
    let dataset = id(1);
    activity.pass_started(dataset, 1);
    activity.document_progress(dataset, 44, Some(49), 2);
    activity.pass_refused(dataset, "sync_peer_closed", 3, &[]);
    activity.pass_started(dataset, 4);
    activity.document_progress(dataset, 10, Some(49), 5);
    let pass = activity.dataset(dataset).unwrap();
    assert_eq!(pass.documents_synced, 10);
    assert_eq!(pass.confirmed_documents.as_ref().unwrap().synced, 44);
    assert!(!pass.confirmed_documents.as_ref().unwrap().complete);
    activity.document_progress(dataset, 49, None, 6);
    activity.pass_finished(dataset, PassCounts::default(), 7);
    activity.pass_started(dataset, 8);
    let pass = activity.dataset(dataset).unwrap();
    assert_eq!(pass.documents_synced, 0);
    assert_eq!(pass.finished_at, None);
    assert_eq!(
        pass.confirmed_documents.unwrap(),
        CloudDocumentProgress {
            required: 49,
            synced: 49,
            complete: true,
        }
    );
}

#[test]
fn an_edit_discards_retained_progress_until_a_new_pass() {
    let mut activity = CloudActivity::new();
    let dataset = id(1);
    activity.pass_started(dataset, 1);
    activity.document_progress(dataset, 49, Some(49), 2);
    activity.pass_finished(dataset, PassCounts::default(), 3);
    activity.documents_changed(dataset);
    activity.document_progress(dataset, 49, None, 4);
    activity.pass_finished(dataset, PassCounts::default(), 5);
    assert!(
        activity
            .dataset(dataset)
            .unwrap()
            .confirmed_documents
            .is_none()
    );
    activity.pass_started(dataset, 6);
    activity.document_progress(dataset, 1, Some(50), 7);
    assert_eq!(
        activity
            .dataset(dataset)
            .unwrap()
            .confirmed_documents
            .unwrap(),
        CloudDocumentProgress {
            required: 50,
            synced: 1,
            complete: false,
        }
    );
}

#[test]
fn a_changed_inventory_or_failed_final_proof_can_reduce_progress() {
    let mut activity = CloudActivity::new();
    let dataset = id(1);
    activity.pass_started(dataset, 1);
    activity.document_progress(dataset, 49, Some(49), 2);
    activity.pass_finished(dataset, PassCounts::default(), 3);
    activity.pass_started(dataset, 4);
    activity.document_progress(dataset, 1, Some(50), 5);
    assert_eq!(
        activity
            .dataset(dataset)
            .unwrap()
            .confirmed_documents
            .unwrap()
            .synced,
        1
    );
    activity.document_progress(dataset, 50, None, 6);
    activity.document_progress(dataset, 48, None, 7);
    let confirmed = activity
        .dataset(dataset)
        .unwrap()
        .confirmed_documents
        .unwrap();
    assert_eq!(confirmed.synced, 48);
    assert!(!confirmed.complete);
}

#[test]
fn refusals_deferrals_and_remote_hints_prevent_retained_completion() {
    for counts in [
        PassCounts {
            deferred: 1,
            ..Default::default()
        },
        PassCounts {
            remote_hint: true,
            ..Default::default()
        },
    ] {
        let mut activity = CloudActivity::new();
        let dataset = id(1);
        activity.pass_started(dataset, 1);
        activity.document_progress(dataset, 49, Some(49), 2);
        activity.pass_finished(dataset, PassCounts::default(), 3);
        activity.pass_started(dataset, 4);
        activity.document_progress(dataset, 49, Some(49), 5);
        activity.pass_finished(dataset, counts, 6);
        activity.pass_started(dataset, 7);
        assert!(
            !activity
                .dataset(dataset)
                .unwrap()
                .confirmed_documents
                .unwrap()
                .complete
        );
        activity.document_progress(dataset, 49, Some(49), 8);
        activity.pass_finished(dataset, PassCounts::default(), 9);
        activity.pass_refused(dataset, "sync_peer_closed", 10, &[]);
        activity.pass_started(dataset, 11);
        assert!(
            !activity
                .dataset(dataset)
                .unwrap()
                .confirmed_documents
                .unwrap()
                .complete
        );
    }
}

/// Twenty passes over one document are one entry naming the newest facts —
/// the `Store::committed` property, which is what keeps this bounded at all.
#[test]
fn many_passes_over_one_document_are_one_entry() {
    let mut activity = CloudActivity::new();
    let dataset = id(1);
    let key = (id(2), id(3));
    for pass in 0..20 {
        let at = 1_000 + pass as i64;
        activity.pass_started(dataset, at);
        activity.unit(dataset, Some(key), UnitStep::Started, at);
        activity.unit(dataset, Some(key), UnitStep::Uploaded, at);
        activity.unit(dataset, Some(key), UnitStep::Settled, at);
        activity.pass_finished(
            dataset,
            PassCounts {
                settled: 1,
                uploaded: 1,
                ..PassCounts::default()
            },
            at,
        );
    }
    assert_eq!(activity.documents_held(), 1);
    assert_eq!(activity.datasets_held(), 1);
    let facts = activity.document(key.0, key.1).expect("a record");
    assert_eq!(
        facts.last_uploaded_at,
        Some(1_019),
        "the newest, not the first"
    );
    assert_eq!(facts.last_settled_at, Some(1_019));
    assert_eq!(facts.last_downloaded_at, None, "no fact is not zero");
    let pass = activity.dataset(dataset).expect("a pass");
    assert_eq!(
        pass.settled, 1,
        "the last pass's count, never a running total"
    );
    assert_eq!(pass.finished_at, Some(1_019));
}

/// A new pass does not report the previous one's totals while it runs.
#[test]
fn a_started_pass_clears_the_previous_counts() {
    let mut activity = CloudActivity::new();
    let dataset = id(1);
    activity.pass_started(dataset, 10);
    activity.pass_finished(
        dataset,
        PassCounts {
            settled: 9,
            uploaded: 1,
            remote_hint: true,
            ..PassCounts::default()
        },
        11,
    );
    activity.pass_started(dataset, 20);
    let pass = activity.dataset(dataset).expect("a pass");
    assert_eq!(pass.settled, 0);
    assert_eq!(pass.uploaded, 0);
    assert!(!pass.remote_hint);
    assert_eq!(pass.started_at, Some(20));
    assert_eq!(pass.finished_at, None, "it has not finished");
}

/// More documents than the ceiling evicts the least recently touched, and
/// never the one that just moved.
#[test]
fn the_document_record_stays_bounded() {
    let mut activity = CloudActivity::new();
    let dataset = id(1);
    let workspace = id(2);
    for n in 0..(MAX_DOCUMENTS as u128 + 50) {
        let document = Uuid::from_u128(n + 1);
        activity.unit(
            dataset,
            Some((workspace, document)),
            UnitStep::Uploaded,
            n as i64,
        );
    }
    assert!(
        activity.documents_held() <= MAX_DOCUMENTS,
        "{} entries held",
        activity.documents_held()
    );
    let newest = Uuid::from_u128(MAX_DOCUMENTS as u128 + 50);
    assert!(
        activity.document(workspace, newest).is_some(),
        "the newest entry survives"
    );
    assert!(
        activity.document(workspace, Uuid::from_u128(1)).is_none(),
        "the oldest was evicted"
    );
}

/// The same ceiling on datasets, which is the live-permit bound restated.
#[test]
fn the_dataset_record_stays_bounded() {
    let mut activity = CloudActivity::new();
    for n in 0..(MAX_DATASETS as u128 + 10) {
        activity.pass_started(Uuid::from_u128(n + 1), n as i64);
    }
    assert!(activity.datasets_held() <= MAX_DATASETS);
    assert!(
        activity
            .dataset(Uuid::from_u128(MAX_DATASETS as u128 + 10))
            .is_some()
    );
}

/// A Profile unit is a fact about the dataset and about no document.
#[test]
fn a_profile_unit_records_no_document() {
    let mut activity = CloudActivity::new();
    let dataset = id(7);
    activity.unit(dataset, None, UnitStep::Uploaded, 5);
    assert_eq!(activity.documents_held(), 0);
    assert_eq!(activity.dataset(dataset).expect("a pass").uploaded, 1);
}

/// A refusal is the answer to "what is this document waiting on", so it lands
/// on every document the pass had started as well as on the dataset.
#[test]
fn a_refusal_reaches_the_documents_the_pass_touched() {
    let mut activity = CloudActivity::new();
    let dataset = id(1);
    let one = (id(2), id(3));
    let two = (id(2), id(4));
    activity.pass_started(dataset, 100);
    activity.unit(dataset, Some(one), UnitStep::Started, 101);
    activity.unit(dataset, Some(two), UnitStep::Started, 102);
    activity.pass_refused(dataset, "sync_transfer_timeout", 103, &[one, two]);

    for key in [one, two] {
        let facts = activity.document(key.0, key.1).expect("a record");
        assert_eq!(
            facts.last_refusal_code.as_deref(),
            Some("sync_transfer_timeout")
        );
        assert_eq!(facts.last_refused_at, Some(103));
    }
    let pass = activity.dataset(dataset).expect("a pass");
    assert_eq!(pass.refusal_code.as_deref(), Some("sync_transfer_timeout"));
    assert_eq!(pass.finished_at, Some(103));
}

/// A pass that finishes after a refused one clears the refusal rather than
/// leaving a stale code beside a fresh success.
#[test]
fn a_finished_pass_clears_the_last_refusal() {
    let mut activity = CloudActivity::new();
    let dataset = id(1);
    activity.pass_refused(dataset, "sync_unavailable", 10, &[]);
    activity.pass_started(dataset, 20);
    activity.pass_finished(dataset, PassCounts::default(), 21);
    let pass = activity.dataset(dataset).expect("a pass");
    assert_eq!(pass.refusal_code, None);
    assert_eq!(pass.refused_at, None);
}

/// Nothing is invented: a document no pass has visited has no record.
#[test]
fn an_unvisited_document_has_no_record() {
    let activity = CloudActivity::new();
    assert!(activity.document(id(1), id(2)).is_none());
    assert!(activity.dataset(id(1)).is_none());
}

/// A pass that finished with a unit out of deferrals is a finished pass that
/// still names its trouble. Both halves matter: the counts are true, so a
/// reader can see what did move, and the code is there, so nobody reads the
/// finish as "everything synced".
#[test]
fn a_unit_that_stopped_syncing_is_named_on_a_pass_that_finished() {
    let mut activity = CloudActivity::new();
    let dataset = id(1);
    let stuck = (id(2), id(3));
    activity.pass_started(dataset, 10);
    activity.unit(dataset, Some(stuck), UnitStep::Deferred, 11);
    activity.pass_finished(
        dataset,
        PassCounts {
            settled: 4,
            deferred: 1,
            ..PassCounts::default()
        },
        12,
    );
    activity.unit_unavailable(dataset, Some(stuck), "cloud_unit_unavailable", 12);

    let pass = activity.dataset(dataset).expect("a pass");
    assert_eq!(pass.finished_at, Some(12), "the pass did finish");
    assert_eq!(pass.settled, 4, "and its counts are true");
    assert_eq!(
        pass.refusal_code.as_deref(),
        Some("cloud_unit_unavailable"),
        "and it is not reported as a clean finish"
    );
    let facts = activity.document(stuck.0, stuck.1).expect("a record");
    assert_eq!(
        facts.last_refusal_code.as_deref(),
        Some("cloud_unit_unavailable")
    );
    assert_eq!(facts.last_refused_at, Some(12));
}
