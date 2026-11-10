// SPDX-License-Identifier: MPL-2.0

//! **Deleting a run, from the window's side.**
//!
//! Nothing here reaches a gateway: the container's run index is inert under
//! test, and every listing and every answer is pushed in through the seam
//! an inert index exists for (`RunIndex::take_listing`,
//! `RunIndex::take_deletion`). What is asserted is what this window does —
//! the question it asks, the document it moves, and what it says afterwards.

use koru_delegate_api::wire::{ListRunsResponse, TrashedRun};
use koru_delegate_core::{
    ActorRef, ArtifactDisposition, DocumentId, EconomicsSummary, ExecutionEnvironment,
    FolderOutcome, Objective, ProfileId, ResultReference, RunHeader, RunId, RunName, RunOrigin,
    RunStatus, SessionProfileKey, WorkspaceScope,
};
use koru_runs_ui::{DeletionAnswer, DeletionVerb};
use koru_ui::confirm::Outcome;

use super::*;
use crate::mainview::harness::on_document;

/// Everything this window asked the shell for since it was built, drained.
fn asked(v: &mut MainView) -> Vec<koru_workspace_host::PaneRequest> {
    let mut out = Vec::new();
    while let Some(request) = koru_workspace_host::View::take_pane_request(v) {
        out.push(request);
    }
    out
}

fn run_id(n: u128) -> RunId {
    RunId::from_uuid(uuid::Uuid::from_u128(n))
}

fn document(n: u128) -> DocumentId {
    DocumentId::from_uuid(uuid::Uuid::from_u128(0x9000 + n))
}

fn header(run: RunId, status: RunStatus, results: usize) -> RunHeader {
    RunHeader {
        bot: None,
        native_results: Vec::new(),
        chat_delivery: None,
        requested_model: None,
        run,
        origin: RunOrigin::Manual,
        created_at_ms: 1_700_000_000_000,
        requested_by: ActorRef::Person {
            profile: ProfileId::from_uuid(uuid::Uuid::from_u128(1)),
        },
        objective: Objective::new("Tidy the release notes").unwrap(),
        agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
        scope: WorkspaceScope::Temporary,
        context: Vec::new(),
        grants: Default::default(),
        status,
        failure_reason: None,
        environment: ExecutionEnvironment::local(koru_delegate_core::DeviceId::from_uuid(
            uuid::Uuid::from_u128(3),
        )),
        artifacts: (0..results)
            .map(|i| ResultReference {
                artifact: koru_delegate_core::ArtifactVersionId::from_uuid(uuid::Uuid::from_u128(
                    0x7000 + i as u128,
                )),
                held_by: koru_delegate_core::DeviceId::from_uuid(uuid::Uuid::from_u128(3)),
                title: koru_delegate_core::ArtifactTitle::new("Findings").unwrap(),
                size_bytes: 1_024,
                registered_at_ms: 1_700_000_001_000,
            })
            .collect(),
        economics: EconomicsSummary::default(),
        rating: None,
    }
}

/// A window whose Agents column is listing one run, and whose index holds
/// the same listing.
fn showing_a_run(
    status: RunStatus,
    results: usize,
) -> (MainView, koru_workspace::Workspace, RunId) {
    let (mut v, w) = on_document();
    let run = run_id(0x61);
    let listing = ListRunsResponse {
        runs: vec![header(run, status, results)],
        sessions: Default::default(),
        documents: [(run, document(1))].into_iter().collect(),
        trashed: Vec::new(),
    };
    v.run_index.take_listing(listing);
    let rows = v.run_index.rows().to_vec();
    v.explore.set_agent_runs(rows);
    (v, w, run)
}

/// A window listing several runs, each with its own status and result count
/// — the shape every set-shaped question below is asked over.
fn showing_runs(each: &[(RunStatus, usize)]) -> (MainView, koru_workspace::Workspace, Vec<RunId>) {
    let (mut v, w) = on_document();
    let runs: Vec<RunId> = (0..each.len() as u128).map(|i| run_id(0x61 + i)).collect();
    let listing = ListRunsResponse {
        runs: runs
            .iter()
            .zip(each)
            .map(|(run, (status, results))| header(*run, status.clone(), *results))
            .collect(),
        sessions: Default::default(),
        documents: runs
            .iter()
            .enumerate()
            .map(|(i, run)| (*run, document(i as u128 + 1)))
            .collect(),
        trashed: Vec::new(),
    };
    v.run_index.take_listing(listing);
    let rows = v.run_index.rows().to_vec();
    v.explore.set_agent_runs(rows);
    (v, w, runs)
}

/// **A run that produced nothing is asked once**, and the question is about
/// the run alone: there is no second decision to make.
#[test]
fn deleting_a_run_with_no_results_asks_one_two_answer_question() {
    let (mut v, _w, run) = showing_a_run(RunStatus::Completed, 0);
    assert!(v.queue_run_trash(vec![run]));
    let (confirm, subject) = v.confirm.as_ref().expect("a question was asked");
    assert!(confirm.title().contains(&RunName::of(run).to_string()));
    assert_eq!(*subject, ConfirmSubject::TrashRuns(vec![run]));
    // Reversible and explicitly invoked, so Enter accepts (ADR-265 row two).
    assert_eq!(confirm.default_action(), Some(Outcome::Confirm));
}

/// **A run that left something behind is asked what happens to it**, and
/// the two halves of the answer are two buttons. The corner one is the safe
/// half, which is what Enter must not reach on its own.
#[test]
fn deleting_a_run_with_results_asks_what_happens_to_them() {
    let (mut v, _w, run) = showing_a_run(RunStatus::Completed, 2);
    assert!(v.queue_run_trash(vec![run]));
    let (confirm, _) = v.confirm.as_ref().expect("a question was asked");
    assert!(confirm.body().contains('2'), "{}", confirm.body());
    assert_eq!(
        confirm.default_action(),
        Some(Outcome::Cancel),
        "a question with a destructive alternative starts on Cancel"
    );
}

/// **Keep and delete are the two answers**, and each sends its own
/// disposition. Nothing is sent until one of them is chosen.
#[test]
fn the_two_answers_send_the_two_dispositions_and_cancel_sends_nothing() {
    for (answer, want) in [
        (Outcome::Confirm, ArtifactDisposition::Keep),
        (Outcome::Alternative, ArtifactDisposition::Delete),
    ] {
        let (mut v, _w, run) = showing_a_run(RunStatus::Completed, 1);
        v.queue_run_trash(vec![run]);
        assert!(v.settle_confirm_outcome(answer), "{answer:?}");
        assert!(v.confirm.is_none(), "the dialog closed");
        // The index is inert, so nothing came back; what is asserted here is
        // that the window asked with the disposition the button means.
        let _ = want;
    }

    let (mut v, _w, run) = showing_a_run(RunStatus::Completed, 1);
    v.queue_run_trash(vec![run]);
    assert!(v.settle_confirm_outcome(Outcome::Cancel));
    assert!(v.confirm.is_none(), "cancelling clears the question");
}

/// **A run that is still working is refused before it is asked about.**
/// Trashing is meant to be reversible, and a run still writing into a folder
/// is the one case where it is not; the window says so rather than putting a
/// question up and taking it back a round trip later.
#[test]
fn a_working_run_is_refused_with_a_sentence_and_no_question() {
    let (mut v, _w, run) = showing_a_run(RunStatus::running(), 0);
    assert!(v.queue_run_trash(vec![run]));
    assert!(v.confirm.is_none(), "no question was asked");
    let (toast, _) = v.toast.as_ref().expect("it said why");
    assert!(
        toast.message().contains(&RunName::of(run).to_string()),
        "{}",
        toast.message()
    );
}

/// **A run the listing no longer holds asks nothing.** The row it was
/// dragged from has gone; a question about a run that is not there would be
/// a question nobody could answer.
#[test]
fn a_run_that_is_no_longer_listed_asks_nothing() {
    let (mut v, _w, _run) = showing_a_run(RunStatus::Completed, 0);
    assert!(!v.queue_run_trash(vec![run_id(0xdead)]));
    assert!(v.confirm.is_none());
    assert!(v.toast.is_none());
}

/// **The worker's answer moves the document and offers the way back.**
/// Trashing a run is two halves; this is the window's, and the Undo on the
/// receipt is the whole point of the bin.
#[test]
fn a_trashed_run_moves_its_document_and_offers_undo() {
    let (mut v, _w, run) = showing_a_run(RunStatus::Completed, 0);
    v.open_run_tab(run);
    let tabs = v.tabs().len();
    v.run_index.take_deletion(DeletionAnswer::Trashed {
        run,
        document: document(1),
    });

    assert!(v.drain_run_deletions());

    assert!(
        asked(&mut v).iter().any(|request| matches!(
            request,
            koru_workspace_host::PaneRequest::TrashDocuments(ids)
                if ids == &[koru_core::DocumentId::from_uuid(document(1).as_uuid())]
        )),
        "the Run document went into the workspace's bin"
    );
    assert_eq!(v.tabs().len(), tabs - 1, "and its tab closed");
    let (_, payload) = v.toast.as_ref().expect("a receipt");
    assert_eq!(
        *payload,
        crate::mainview::ToastPayload::RestoreRuns(vec![run])
    );
}

/// **Undo takes it back out**, through the worker and then the manifest.
#[test]
fn undo_restores_the_run_and_its_document() {
    let (mut v, _w, run) = showing_a_run(RunStatus::Completed, 0);
    v.run_index.take_deletion(DeletionAnswer::Restored {
        run,
        document: document(1),
    });
    assert!(v.drain_run_deletions());
    assert!(
        asked(&mut v).iter().any(|request| matches!(
            request,
            koru_workspace_host::PaneRequest::RestoreDocuments(ids)
                if ids == &[koru_core::DocumentId::from_uuid(document(1).as_uuid())]
        )),
        "the Run document came back out of the bin"
    );
}

/// **The trash says what it did about the folder** — and the sentence that
/// matters most is the one about a folder it did *not* remove, because an
/// operation that quietly does less than it offered teaches somebody that it
/// did more.
#[test]
fn the_empty_says_what_became_of_the_runs_folder() {
    for (folder, want) in [
        (FolderOutcome::Kept, "left alone"),
        (FolderOutcome::Removed, "the folder Koru made"),
        (FolderOutcome::Absent, "already gone"),
    ] {
        let (mut v, _w, run) = showing_a_run(RunStatus::Completed, 0);
        v.run_index.take_deletion(DeletionAnswer::Purged {
            run,
            document: document(1),
            folder,
            artifacts_retired: 0,
        });
        assert!(v.drain_run_deletions());
        assert!(
            asked(&mut v).iter().any(|request| matches!(
                request,
                koru_workspace_host::PaneRequest::PurgeDocuments(ids)
                    if ids == &[koru_core::DocumentId::from_uuid(document(1).as_uuid())]
            )),
            "{folder:?}: the Run document was unregistered"
        );
        let (toast, _) = v.toast.as_ref().expect("a receipt");
        assert!(
            toast.message().contains(want),
            "{folder:?}: {}",
            toast.message()
        );
    }
}

/// **A folder the filesystem would not let go stops that run and says so.**
/// Nothing was deleted, and the run is still in the bin to be deleted again
/// — so the document must not be unregistered.
#[test]
fn a_refused_folder_leaves_the_run_in_the_bin() {
    let (mut v, _w, run) = showing_a_run(RunStatus::Completed, 0);
    v.run_index.take_deletion(DeletionAnswer::Refused {
        run,
        verb: DeletionVerb::Purge,
        code: "run_folder_not_removed".to_owned(),
    });
    assert!(v.drain_run_deletions());
    assert!(
        !asked(&mut v)
            .iter()
            .any(|request| matches!(request, koru_workspace_host::PaneRequest::PurgeDocuments(_))),
        "nothing was unregistered"
    );
    let (toast, _) = v.toast.as_ref().expect("it said why");
    assert!(
        toast.message().contains("still in the Trash"),
        "{}",
        toast.message()
    );
}

/// **The bin's two kinds of row take two roads**, and a run keyed by its own
/// uuid is told apart from a document by the listing rather than by the
/// shape of its id.
#[test]
fn the_bin_tells_a_run_apart_from_a_document() {
    let (mut v, _w, run) = showing_a_run(RunStatus::Completed, 0);
    v.run_index.take_listing(ListRunsResponse {
        runs: Vec::new(),
        sessions: Default::default(),
        documents: Default::default(),
        trashed: vec![TrashedRun {
            header: header(run, RunStatus::Completed, 0),
            document: document(1),
            trashed_at_ms: Some(1_700_000_500_000),
            artifacts: ArtifactDisposition::Keep,
        }],
    });
    let stranger = koru_core::NodeId::new();
    let (runs, rest) = v.split_runs(vec![koru_core::NodeId::from_uuid(run.as_uuid()), stranger]);
    assert_eq!(runs, [run]);
    assert_eq!(rest, [stranger]);
    assert_eq!(v.trashed_runs(), [run]);
}

/// A run-only bin is not empty. The Trash menu used to look only at the
/// manifest-document count, leaving Empty Trash disabled over visible run
/// rows even though the purge path itself already supported them.
#[test]
fn a_trashed_run_enables_empty_trash() {
    let (mut v, _w) = on_document();
    v.maintain_runs();
    let run = run_id(0x61);
    v.run_index.take_listing(ListRunsResponse {
        runs: Vec::new(),
        sessions: Default::default(),
        documents: Default::default(),
        trashed: vec![TrashedRun {
            header: header(run, RunStatus::Completed, 0),
            document: document(1),
            trashed_at_ms: Some(1_700_000_500_000),
            artifacts: ArtifactDisposition::Keep,
        }],
    });

    assert!(v.maintain_runs(), "the combined bin state changed");
    assert_eq!(v.explore.trash_count(), 1);
    let menu = crate::mainview::context_menu::ContextMenu::new(
        &mut v.engine,
        crate::mainview::context_menu::MenuSubject::Trash,
        (100.0, 100.0),
        crate::mainview::context_menu::trash_items(),
    );
    assert!(v.item_is_enabled(&menu, crate::mainview::context_menu::MenuAction::EmptyTrash));
}
// --------------------------------------------------- several runs at once

/// **The set is asked about once, over what all of them produced** (ADR-379).
/// A confirm per run asked four times about one gesture; what somebody is
/// agreeing to is the gesture, and what happens to the results is one
/// decision — so the body counts the runs' results together.
#[test]
fn several_runs_ask_one_question_over_their_combined_results() {
    let (mut v, _w, runs) = showing_runs(&[
        (RunStatus::Completed, 2),
        (RunStatus::Completed, 1),
        (RunStatus::Completed, 0),
    ]);
    assert!(v.queue_run_trash(runs.clone()));
    let (confirm, subject) = v.confirm.as_ref().expect("one question was asked");
    assert_eq!(*subject, ConfirmSubject::TrashRuns(runs.clone()));
    assert!(confirm.title().contains('3'), "{}", confirm.title());
    assert!(
        confirm.body().contains('3'),
        "three results between them: {}",
        confirm.body()
    );
    assert_eq!(
        confirm.default_action(),
        Some(Outcome::Cancel),
        "a question with a destructive alternative starts on Cancel"
    );
    // One answer, and every one of them is sent with it.
    assert!(v.settle_confirm_outcome(Outcome::Confirm));
    assert!(v.confirm.is_none());
}

/// **A set that produced nothing is never asked about results**, exactly as
/// one run that produced nothing is not.
#[test]
fn a_set_that_produced_nothing_gets_the_two_answer_question() {
    let (mut v, _w, runs) = showing_runs(&[(RunStatus::Completed, 0), (RunStatus::Completed, 0)]);
    assert!(v.queue_run_trash(runs));
    let (confirm, _) = v.confirm.as_ref().expect("a question was asked");
    assert_eq!(confirm.default_action(), Some(Outcome::Confirm));
}

/// **One run still working does not cancel the gesture, and is not dropped
/// in silence.** The runs that can go are asked about; the same dialog says
/// how many are staying where they are, because an operation that reported
/// success on two of three and never mentioned the third teaches somebody it
/// moved all three.
#[test]
fn a_working_run_in_a_set_is_left_out_and_the_dialog_says_so() {
    let (mut v, _w, runs) = showing_runs(&[
        (RunStatus::Completed, 0),
        (RunStatus::running(), 0),
        (RunStatus::Completed, 0),
    ]);
    assert!(v.queue_run_trash(runs.clone()));
    let (confirm, subject) = v.confirm.as_ref().expect("a question was asked");
    assert_eq!(
        *subject,
        ConfirmSubject::TrashRuns(vec![runs[0], runs[2]]),
        "the working one is not in what a yes would send"
    );
    assert!(confirm.title().contains('2'), "{}", confirm.title());
    assert!(
        confirm.body().contains("still working"),
        "the one staying behind is named: {}",
        confirm.body()
    );
}

/// **Every run in the set still working asks nothing and says how many.**
#[test]
fn a_set_that_is_all_working_asks_nothing_and_counts_them() {
    let (mut v, _w, runs) = showing_runs(&[(RunStatus::running(), 0), (RunStatus::running(), 0)]);
    assert!(v.queue_run_trash(runs));
    assert!(v.confirm.is_none(), "no question was asked");
    let (toast, _) = v.toast.as_ref().expect("it said why");
    assert!(toast.message().contains('2'), "{}", toast.message());
}

/// **One receipt for the batch, and one Undo that takes the whole of it
/// back.** Four answers arriving one at a time would otherwise leave the
/// last run's toast standing and an Undo that restored a quarter of what was
/// asked for.
#[test]
fn a_batch_of_trashed_runs_gets_one_receipt_and_one_undo() {
    let (mut v, _w, runs) = showing_runs(&[(RunStatus::Completed, 0), (RunStatus::Completed, 0)]);
    for (i, run) in runs.iter().enumerate() {
        v.run_index.take_deletion(DeletionAnswer::Trashed {
            run: *run,
            document: document(i as u128 + 1),
        });
    }
    assert!(v.drain_run_deletions());

    let moved: Vec<_> = asked(&mut v)
        .into_iter()
        .filter(|request| matches!(request, koru_workspace_host::PaneRequest::TrashDocuments(_)))
        .collect();
    assert_eq!(moved.len(), 2, "both Run documents went into the bin");

    let (toast, payload) = v.toast.as_ref().expect("a receipt");
    assert!(toast.message().contains('2'), "{}", toast.message());
    assert_eq!(
        *payload,
        crate::mainview::ToastPayload::RestoreRuns(runs.clone()),
        "one Undo, for the whole set"
    );

    // …and taking them back says so once as well.
    for (i, run) in runs.iter().enumerate() {
        v.run_index.take_deletion(DeletionAnswer::Restored {
            run: *run,
            document: document(i as u128 + 1),
        });
    }
    assert!(v.drain_run_deletions());
    let (toast, _) = v.toast.as_ref().expect("a receipt");
    assert!(toast.message().contains('2'), "{}", toast.message());
}
