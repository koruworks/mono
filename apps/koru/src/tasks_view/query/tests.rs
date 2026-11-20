// SPDX-License-Identifier: MPL-2.0

//! The wait: what a surface does with a write's receipt, driven with a clock
//! and answers it is handed rather than a store.
//!
//! **Nothing here dials anything.** A panel with no client starts no thread,
//! so `maintain` past a deadline is a no-op on the wire and the wait is
//! exercised through the seams the worker would come back through.

use super::*;

/// One writing session, so the tests below vary the operation rather than the
/// writer. `at_peer` is for the cases that are about two writers.
const PEER: u64 = 1;

fn at(
    workspace: koru_store_core::ids::WorkspaceId,
    document: koru_store_core::ids::DocumentId,
    n: i64,
) -> WriteRequirement {
    at_peer(workspace, document, PEER, n)
}

fn at_peer(
    workspace: koru_store_core::ids::WorkspaceId,
    document: koru_store_core::ids::DocumentId,
    peer: u64,
    n: i64,
) -> WriteRequirement {
    WriteRequirement {
        workspace,
        document,
        peer,
        counter: n,
    }
}

fn ids() -> (
    koru_store_core::ids::WorkspaceId,
    koru_store_core::ids::DocumentId,
) {
    (
        koru_store_core::ids::WorkspaceId::new(),
        koru_store_core::ids::DocumentId::new(),
    )
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

// ------------------------------------------------------------- the policy

/// The first question after a write waits the store's quiet window: nothing a
/// client does can make the projection sooner, so an earlier ask is a wasted
/// round trip answered from the previous projection.
#[test]
fn the_first_ask_after_a_write_waits_the_stores_quiet_window() {
    let (w, d) = ids();
    let mut wait = Wait::default();
    let t0 = Instant::now();
    assert_eq!(
        wait.require(at(w, d, 5), t0),
        Some(t0 + first_ask_after_write())
    );
    assert_eq!(first_ask_after_write(), ms(PROJECTION_QUIET_MILLIS as u64));
    assert!(wait.is_waiting());
}

/// **The ceiling is derived, not a literal.** Tuning the projector moves it.
#[test]
fn the_ceiling_is_the_stores_hold_plus_the_allowance() {
    assert_eq!(
        wait_ceiling(),
        ms((PROJECTION_HOLD_CEILING_MILLIS + TRANSPORT_ALLOWANCE_MILLIS) as u64)
    );
    assert!(wait_ceiling() > ms(PROJECTION_HOLD_CEILING_MILLIS as u64));
}

/// An answer to a request sent **before** the write — carrying nothing — can
/// arrive after it, saying *covered*. It covered nothing this wait is about,
/// and must not clear it.
#[test]
fn an_old_empty_answer_arriving_after_a_write_clears_nothing() {
    let (w, d) = ids();
    let mut wait = Wait::default();
    let t0 = Instant::now();
    wait.require(at(w, d, 5), t0);
    let outcome = wait.acknowledge(&[], Some(true), t0 + ms(100));
    assert_eq!(outcome, WaitOutcome::Retry(t0 + ms(100 + RETRY_MILLIS)));
    assert_eq!(wait.wanted().len(), 1, "B is still waited on");
}

/// Edit A, then edit B while A's request is in flight. A's answer covers A
/// and says so; B stays.
#[test]
fn an_answer_covering_a_does_not_clear_b() {
    let (w, d) = ids();
    let (_, other) = ids();
    let mut wait = Wait::default();
    let t0 = Instant::now();
    wait.require(at(w, d, 5), t0);
    let carried = wait.carrying();
    wait.require(at(w, other, 1), t0 + ms(50));
    assert_eq!(wait.wanted().len(), 2);

    let outcome = wait.acknowledge(&carried, Some(true), t0 + ms(600));
    assert!(matches!(outcome, WaitOutcome::Retry(_)));
    assert_eq!(wait.wanted().len(), 1);
    assert_eq!(wait.wanted()[0].document, other, "A is satisfied, B is not");
}

/// Two writes to one document are one requirement — the union — and an
/// answer covering the first alone does not cover it.
#[test]
fn two_writes_to_one_document_are_one_requirement_the_union() {
    let (w, d) = ids();
    let mut wait = Wait::default();
    let t0 = Instant::now();
    wait.require(at(w, d, 5), t0);
    let first = wait.carrying();
    wait.require(at(w, d, 7), t0 + ms(10));
    assert_eq!(wait.wanted().len(), 1, "coalesced per document");
    assert_eq!(wait.wanted()[0].counter, 7, "the later operation wins");

    let outcome = wait.acknowledge(&first, Some(true), t0 + ms(600));
    assert!(
        matches!(outcome, WaitOutcome::Retry(_)),
        "the answer had {{1:5}}; the wait is on {{1:7}}"
    );
    assert_eq!(wait.wanted().len(), 1);

    let later = wait.carrying();
    let outcome = wait.acknowledge(&later, Some(true), t0 + ms(900));
    assert!(matches!(outcome, WaitOutcome::Covered { .. }));
    assert!(!wait.is_waiting());
}

/// An answer that includes *more* than was required covers it — the
/// coalescing case, on the client side.
#[test]
fn an_answer_ahead_of_the_requirement_covers_it() {
    let (w, d) = ids();
    let mut wait = Wait::default();
    let t0 = Instant::now();
    wait.require(at(w, d, 5), t0);
    // The request carried {1:5}; the wait moved on to nothing newer; the
    // store answered from {1:9}. `covered: true` is what the store says, and
    // the carried set is what is acknowledged.
    let carried = wait.carrying();
    assert!(matches!(
        wait.acknowledge(&carried, Some(true), t0 + ms(600)),
        WaitOutcome::Covered { .. }
    ));
}

/// **A retry never resets the deadline; a new write extends it.**
#[test]
fn a_retry_keeps_the_deadline_and_a_new_write_moves_it() {
    let (w, d) = ids();
    let mut wait = Wait::default();
    let t0 = Instant::now();
    wait.require(at(w, d, 5), t0);
    let carried = wait.carrying();
    let ceiling = wait_ceiling();

    for step in [500u64, 1_000, 2_000, 4_000] {
        assert!(
            matches!(
                wait.acknowledge(&carried, Some(false), t0 + ms(step)),
                WaitOutcome::Retry(_)
            ),
            "still inside the ceiling at {step} ms"
        );
    }
    // The clock did not move for any of those.
    assert!(matches!(
        wait.acknowledge(&carried, Some(false), t0 + ceiling - ms(1)),
        WaitOutcome::Retry(_)
    ));

    // A new write, and the ceiling is its own.
    let t1 = t0 + ms(3_000);
    wait.require(at(w, d, 6), t1);
    let carried = wait.carrying();
    assert!(matches!(
        wait.acknowledge(&carried, Some(false), t0 + ceiling + ms(10)),
        WaitOutcome::Retry(_)
    ));
    assert!(matches!(
        wait.acknowledge(&carried, Some(false), t1 + ceiling),
        WaitOutcome::GaveUp { .. }
    ));
    assert!(!wait.is_waiting(), "and the ordinary schedule resumes");
}

/// The ceiling ends the retries. The answer that ended them is drawn — the
/// caller draws every answer — and the wait is empty.
#[test]
fn the_ceiling_ends_the_retries() {
    let (w, d) = ids();
    let mut wait = Wait::default();
    let t0 = Instant::now();
    wait.require(at(w, d, 5), t0);
    wait.asked();
    wait.asked();
    let carried = wait.carrying();
    let outcome = wait.acknowledge(&carried, Some(false), t0 + wait_ceiling());
    assert_eq!(
        outcome,
        WaitOutcome::GaveUp {
            after: wait_ceiling(),
            asks: 2
        }
    );
    assert!(!wait.is_waiting());
}

/// `None` is *cannot tell*: the mechanism turns off, and a later write is an
/// ordinary invalidation rather than a burst of futile fast retries.
#[test]
fn an_unknown_answer_turns_requirements_off() {
    let (w, d) = ids();
    let mut wait = Wait::default();
    let t0 = Instant::now();
    wait.require(at(w, d, 5), t0);
    let carried = wait.carrying();
    assert_eq!(
        wait.acknowledge(&carried, None, t0 + ms(600)),
        WaitOutcome::Unsupported
    );
    assert!(!wait.is_waiting());
    assert_eq!(
        wait.require(at(w, d, 6), t0 + ms(700)),
        None,
        "no wait, no fast ask"
    );
    assert!(!wait.is_waiting());
    wait.reset_support();
    assert!(
        wait.require(at(w, d, 6), t0 + ms(800)).is_some(),
        "a new store may answer"
    );
}

#[test]
fn an_answer_to_nothing_is_idle() {
    let mut wait = Wait::default();
    assert_eq!(
        wait.acknowledge(&[], Some(true), Instant::now()),
        WaitOutcome::Idle
    );
    assert_eq!(
        wait.acknowledge(&[], None, Instant::now()),
        WaitOutcome::Idle
    );
    assert!(
        !wait.unsupported(),
        "an idle wait learned nothing from `None`"
    );
}

// -------------------------------------------------------------- the panel

fn answered(covered: Option<bool>) -> TaskQueryResponse {
    TaskQueryResponse {
        rows: Vec::new(),
        definitions: Vec::new(),
        tags: Vec::new(),
        groups: Vec::new(),
        projected: true,
        updating_workspaces: Vec::new(),
        limit: 100,
        next_after: None,
        restarted: false,
        covered,
    }
}

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, 6).unwrap()
}

fn a_visible_panel(now: Instant) -> (TaskListPanel, WorkspaceId) {
    let workspace = WorkspaceId::new();
    let mut panel = TaskListPanel::default();
    panel.maintain(
        Some(workspace),
        Some(RailTarget::Bucket(Bucket::Today)),
        true,
        now,
    );
    (panel, workspace)
}

/// The page asks at the store's quiet window, then every retry interval
/// while the answer is behind, then stops at the ceiling with the last
/// answer drawn.
#[test]
fn the_page_re_asks_while_the_answer_is_behind_and_stops_at_the_ceiling() {
    let t0 = Instant::now();
    let (mut panel, workspace) = a_visible_panel(t0);
    let (w, d) = ids();
    panel.require_for_test(at(w, d, 5), t0);
    assert_eq!(
        panel.next_refresh_for_test(),
        Some(t0 + first_ask_after_write()),
        "not before the store could have projected it"
    );

    let carried = panel.wanted_for_test().to_vec();
    let t1 = t0 + ms(600);
    panel.accept_carrying_for_test(&answered(Some(false)), &carried, today(), 0, t1);
    assert!(
        panel.known(),
        "the answer is drawn even though it is behind"
    );
    assert_eq!(
        panel.next_refresh_for_test(),
        Some(t1 + ms(RETRY_MILLIS)),
        "and asked again shortly"
    );

    // The retry fires: with no client nothing is spawned, and the schedule
    // moves to its ordinary half minute, exactly as a real start would have.
    panel.maintain(
        Some(workspace),
        Some(RailTarget::Bucket(Bucket::Today)),
        true,
        t1 + ms(RETRY_MILLIS),
    );

    let t2 = t0 + wait_ceiling();
    panel.accept_carrying_for_test(&answered(Some(false)), &carried, today(), 0, t2);
    assert!(panel.wanted_for_test().is_empty(), "the ceiling ended it");
    assert!(
        panel
            .next_refresh_for_test()
            .is_none_or(|due| due > t2 + ms(RETRY_MILLIS)),
        "no fast retry is scheduled past the ceiling"
    );
}

#[test]
fn a_covered_answer_ends_the_wait_and_the_ordinary_schedule_resumes() {
    let t0 = Instant::now();
    let (mut panel, _) = a_visible_panel(t0);
    let (w, d) = ids();
    panel.require_for_test(at(w, d, 5), t0);
    let carried = panel.wanted_for_test().to_vec();
    panel.accept_carrying_for_test(&answered(Some(true)), &carried, today(), 0, t0 + ms(600));
    assert!(panel.wanted_for_test().is_empty());
}

/// A worker that never answers makes no traffic: with nothing to spawn, the
/// deadline passes and the panel is left on its ordinary schedule holding the
/// wait, which the next answer — or the ceiling — settles.
#[test]
fn a_worker_that_never_answers_makes_no_storm() {
    let t0 = Instant::now();
    let (mut panel, workspace) = a_visible_panel(t0);
    let (w, d) = ids();
    panel.require_for_test(at(w, d, 5), t0);
    let target = Some(RailTarget::Bucket(Bucket::Today));
    for step in 1..=40u64 {
        panel.maintain(Some(workspace), target, true, t0 + ms(step * 250));
    }
    assert!(!panel.known(), "no client, no answer");
    assert_eq!(
        panel.wanted_for_test().len(),
        1,
        "still owed, not forgotten"
    );
    assert!(
        panel
            .next_refresh_for_test()
            .is_some_and(|due| due > t0 + ms(10_000)),
        "and on the half-minute schedule, not a fast loop"
    );
}

/// **A failed query does not spend the wait.**
///
/// It used to clear it, and that threw away the rows this window was drawing
/// over the store's answer on the strength of an answer that never arrived.
/// A store refusing every query — which one did, once the old covering
/// revision outgrew its length limit — then looked exactly like a click that
/// did not register. The write is still owed until an answer says it is
/// included or the deadline gives up on it; a transport that could not ask is
/// neither of those.
#[test]
fn a_failed_worker_leaves_the_write_owed() {
    let t0 = Instant::now();
    let (mut panel, _) = a_visible_panel(t0);
    let (w, d) = ids();
    panel.require_for_test(at(w, d, 5), t0);

    panel.fail_for_test("the store went away");

    assert_eq!(
        panel.wanted_for_test().len(),
        1,
        "the write is still owed after a failure"
    );
}

#[test]
fn a_workspace_change_mid_wait_clears_it() {
    let t0 = Instant::now();
    let (mut panel, _) = a_visible_panel(t0);
    let (w, d) = ids();
    panel.require_for_test(at(w, d, 5), t0);
    panel.maintain(
        Some(WorkspaceId::new()),
        Some(RailTarget::Bucket(Bucket::Today)),
        true,
        t0 + ms(100),
    );
    assert!(panel.wanted_for_test().is_empty());
}

#[test]
fn a_hidden_page_waits_on_nothing() {
    let t0 = Instant::now();
    let (mut panel, workspace) = a_visible_panel(t0);
    let (w, d) = ids();
    panel.require_for_test(at(w, d, 5), t0);
    panel.maintain(
        Some(workspace),
        Some(RailTarget::Bucket(Bucket::Today)),
        false,
        t0 + ms(100),
    );
    assert!(panel.wanted_for_test().is_empty());
    assert_eq!(panel.deadline(), None, "and no traffic");
}

/// An older store answers without `covered`; the page takes its ordinary
/// schedule and sends no more requirements to it.
#[test]
fn an_older_store_turns_the_mechanism_off_for_the_page() {
    let t0 = Instant::now();
    let (mut panel, _) = a_visible_panel(t0);
    let (w, d) = ids();
    panel.require_for_test(at(w, d, 5), t0);
    let carried = panel.wanted_for_test().to_vec();
    panel.accept_carrying_for_test(&answered(None), &carried, today(), 0, t0 + ms(600));
    assert!(panel.wanted_for_test().is_empty());
    panel.require_for_test(at(w, d, 6), t0 + ms(700));
    assert!(
        panel.wanted_for_test().is_empty(),
        "no requirement is kept for it"
    );
    assert_eq!(
        panel.next_refresh_for_test(),
        Some(t0 + ms(700)),
        "an ordinary invalidation: asked once, now"
    );
}

/// While the Store's task events are being heard the timer is the net under
/// a missed one, five minutes out; a tail that stops puts the half minute
/// back at once rather than after the slow deadline already set.
#[test]
fn the_timer_stands_back_while_the_store_is_telling_and_returns_when_it_stops() {
    let t0 = Instant::now();
    let (mut panel, workspace) = a_visible_panel(t0);
    let poll = |panel: &mut TaskListPanel, now| {
        panel.maintain(
            Some(workspace),
            Some(RailTarget::Bucket(Bucket::Today)),
            true,
            now,
        );
    };
    poll(&mut panel, t0);
    assert_eq!(panel.next_refresh_for_test(), Some(t0 + REFRESH_EVERY));

    panel.set_event_driven(true);
    let t1 = t0 + REFRESH_EVERY;
    poll(&mut panel, t1);
    assert_eq!(panel.next_refresh_for_test(), Some(t1 + REFRESH_WHEN_TOLD));

    // An event still asks at once: the timer is not the way news arrives.
    panel.invalidate();
    assert!(
        panel
            .next_refresh_for_test()
            .is_some_and(|due| due <= Instant::now())
    );

    poll(&mut panel, Instant::now());
    panel.set_event_driven(false);
    assert!(
        panel
            .next_refresh_for_test()
            .is_some_and(|due| due <= Instant::now() + REFRESH_EVERY),
        "the slow deadline is pulled in when the tail stops"
    );
}
