// SPDX-License-Identifier: MPL-2.0

//! **A delegated run is not followed** (`crate::model::NOT_PROJECTED`, P1.0
//! proposal F4): a commit to one is skipped before the fork, a sweep never
//! compares it, and the source is told only about the commits the projection
//! walks — which is how the daemon keeps it out of automatic history with the
//! same decision.

use super::*;

/// Two hundred steps committed to a run leave the fork count where the page
/// beside it put it, project no row, and reach the source's activity log
/// not once; the page beside it still projects and is still followed.
#[test]
fn a_run_is_skipped_before_the_fork_and_never_followed() {
    let store = FakeStore::default();
    let ws = WorkspaceId::new();
    let page = store.add(ws, "a paragraph");
    let run = store.add_run(ws);
    // The manifest commit that registered both comes first, as it does in
    // the store, so the mirror knows the run's kind before its first step.
    store.announce_manifest(ws);
    let mut p = projector_for(&store, ws);
    settle(&mut p, 0);
    let forks_after_the_sweep = store.forks.borrow().len();
    assert!(
        !store.forks.borrow().contains(&run),
        "the sweep forked the run"
    );

    for i in 0..200 {
        store.step(run, i);
    }
    store.write(page, "another paragraph");
    settle(&mut p, 1_000);

    let forks = store.forks.borrow();
    assert!(!forks.contains(&run), "a step forked the run");
    assert_eq!(
        forks.len(),
        forks_after_the_sweep + 1,
        "exactly one fork happened: the page's"
    );
    assert_eq!(p.stats().skipped, 200);
    assert!(p.projections().revision(run).unwrap().is_none());
    assert!(p.projections().revision(page).unwrap().is_some());

    let activity = store.activity.borrow();
    assert!(
        activity.iter().all(|c| c.document != run),
        "the run reached the activity log"
    );
    assert_eq!(
        activity.iter().filter(|c| c.document == page).count(),
        1,
        "the page's commit did"
    );
}

/// **A reading document is never walked** (OCR TECHNICAL D14): fifty pages
/// of words committed to one fork nothing, project nothing and reach the
/// source's activity log — the door to automatic history — zero times, and a
/// sweep never compares it. The page beside it is still followed.
#[test]
fn a_reading_document_is_skipped_before_the_fork_and_never_reaches_activity() {
    let store = FakeStore::default();
    let ws = WorkspaceId::new();
    let page = store.add(ws, "a paragraph");
    let reading = store.add_reading(ws);
    store.announce_manifest(ws);
    let mut p = projector_for(&store, ws);
    settle(&mut p, 0);
    assert!(
        !store.forks.borrow().contains(&reading),
        "the sweep forked the reading document"
    );

    for page_number in 1..=50 {
        store.read_page(reading, page_number);
    }
    store.write(page, "another paragraph");
    settle(&mut p, 1_000);

    assert!(
        !store.forks.borrow().contains(&reading),
        "a commit forked the reading document"
    );
    assert_eq!(p.stats().skipped, 50);
    assert!(p.projections().revision(reading).unwrap().is_none());
    let activity = store.activity.borrow();
    assert_eq!(
        activity.iter().filter(|c| c.document == reading).count(),
        0,
        "the reading document reached the activity log"
    );
    assert_eq!(activity.iter().filter(|c| c.document == page).count(), 1);
}

/// A sweep over a workspace that holds a run compares every page and never
/// the run — a run has no projection to be stale against, so comparing it
/// would fork it on every sweep for good.
#[test]
fn a_sweep_compares_pages_and_leaves_runs_alone() {
    let store = FakeStore::default();
    let ws = WorkspaceId::new();
    let page = store.add(ws, "a paragraph");
    let run = store.add_run(ws);
    for i in 0..5 {
        store.step(run, i);
    }
    store.committed.borrow_mut().clear();
    // A fresh projector: everything it finds is by comparison.
    let mut p = projector_for(&store, ws);
    settle(&mut p, 0);
    assert_eq!(p.stats().stale_found, 1, "the page, found by comparison");
    assert!(store.forks.borrow().contains(&page));
    assert!(!store.forks.borrow().contains(&run));
    assert_eq!(p.stats().sweeps, 1);
}

/// **The fallback.** A run whose kind the mirror does not know yet — its
/// manifest commit never reached this layer — is treated as any document:
/// forked, adopted, and projected to no rows and no error rather than
/// refused. The proposal's `a_run_document_projects_to_no_rows_and_no_error`
/// is the walk half of this; this is the worker half.
#[test]
fn a_run_the_mirror_does_not_know_yet_is_projected_to_no_rows() {
    let store = FakeStore::default();
    let ws = WorkspaceId::new();
    let run = store.add_run(ws);
    let mut p = Projector::new(&store, Projections::in_memory().unwrap());
    // No manifest commit and no sweep request: the first thing this layer
    // hears is a step.
    store.unreadable.borrow_mut().push(ws);
    store.step(run, 0);
    settle(&mut p, 0);
    assert_eq!(p.stats().skipped, 0);
    assert!(store.forks.borrow().contains(&run));
    let projection = p.projections().read(run).unwrap().expect("projected");
    assert!(projection.nodes.is_empty());
    assert_eq!(p.stats().failed, 0);
}
