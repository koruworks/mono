// SPDX-License-Identifier: MPL-2.0

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};

use koru_core::time::TimeZoneName;
use koru_delegate_api::wire::{
    ListSchedulesResponse, RenameScheduleRequest, RenameScheduleResponse, ScheduleFiringView,
    ScheduleNextFiring, ScheduleView,
};
use koru_delegate_core::{
    ActorRef, DeviceId, DocumentId, Objective, ProfileId, RunId, RunName, ScheduleId,
    SessionProfileKey, WorkspaceId, WorkspaceScope,
};
use koru_delegate_schedule::{
    DelegationTemplate, RecurrenceRule, ScheduleContextFailureStage, ScheduleDefinition,
    ScheduleFiringOutcome, ScheduleHeader, ScheduleLifecycle, SchedulePauseReason, ScheduleState,
};
use koru_i18n::Lang;
use koru_runs_ui::{Motion, RunRow};
use uuid::Uuid;

use super::*;

struct FakeWatch;

impl ScheduleWatch for FakeWatch {}

struct FakeSource {
    listings: Arc<Mutex<VecDeque<Result<ListSchedulesResponse, Failure>>>>,
    handler: Arc<Mutex<Option<WatchHandler>>>,
    opened: std::sync::mpsc::SyncSender<()>,
    lists: Arc<AtomicUsize>,
}

impl ScheduleSource for FakeSource {
    fn list(&self, _workspace: WorkspaceId) -> Result<ListSchedulesResponse, Failure> {
        self.lists.fetch_add(1, Ordering::Relaxed);
        self.listings
            .lock()
            .unwrap()
            .pop_front()
            .expect("the fixture supplied this refresh answer")
    }

    fn watch(
        &self,
        _workspace: WorkspaceId,
        handler: WatchHandler,
    ) -> Result<Box<dyn ScheduleWatch>, Failure> {
        *self.handler.lock().unwrap() = Some(handler);
        self.opened.send(()).unwrap();
        Ok(Box::new(FakeWatch))
    }
}

fn v4(n: u128) -> Uuid {
    Uuid::from_u128(0x00000000_0000_4000_8000_000000000000 | n)
}

fn device(n: u128) -> DeviceId {
    DeviceId::from_uuid(v4(n))
}

fn run(n: u128) -> RunId {
    RunId::from_uuid(v4(n))
}

fn run_row(id: RunId) -> RunRow {
    RunRow {
        run: id,
        name: RunName::of(id).to_string(),
        objective: "fixture".into(),
        status: "Finished".into(),
        needs_attention: false,
        session: None,
        created_at_ms: 1,
        motion: Motion::Still,
        results: 0,
    }
}

fn schedule(n: u128, state: ScheduleState) -> ScheduleView {
    let workspace = WorkspaceId::from_uuid(v4(90));
    let owner = match state {
        ScheduleState::Active { owner }
        | ScheduleState::Paused { owner, .. }
        | ScheduleState::HandoffPending { owner, .. } => owner,
        ScheduleState::Retired => device(1),
    };
    ScheduleView {
        workspace,
        document: DocumentId::from_uuid(v4(100 + n)),
        name: Some(koru_delegate_schedule::ScheduleName::new("Daily review").unwrap()),
        header: ScheduleHeader {
            schedule: ScheduleId::from_uuid(v4(10 + n)),
            created_at_ms: n as i64,
            initial_owner: owner,
            definition: ScheduleDefinition {
                routine: None,
                revision: 1,
                effective_at_ms: 0,
                template: DelegationTemplate {
                    requested_by: ActorRef::Person {
                        profile: ProfileId::from_uuid(v4(80)),
                    },
                    objective: Objective::new("fixture").unwrap(),
                    agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
                    requested_model: None,
                    scope: WorkspaceScope::Temporary,
                    grants: BTreeSet::from([workspace]),
                    context: Vec::new(),
                },
                recurrence: RecurrenceRule::floating("0 9 * * MON-FRI".parse().unwrap()),
                lifecycle: ScheduleLifecycle::Active,
            },
        },
        state,
        next_firing: Some(ScheduleNextFiring::Exact {
            at_ms: 1_767_268_800_000,
        }),
        firings: Vec::new(),
    }
}

fn present(index: &mut ScheduleIndex, lang: Lang, here: DeviceId, runs: Vec<RunRow>) {
    let names = HashMap::from([(here, "Studio".into()), (device(2), "Laptop".into())]);
    index.set_presentation(lang, &TimeZoneName::utc(), Some(here), &names, Some(&runs));
}

#[test]
fn empty_partial_and_failed_lists_are_distinct() {
    let mut index = ScheduleIndex::inert();
    assert_eq!(index.state(), CatalogState::Ready);
    assert!(index.rows().is_empty());

    index.take_listing(ListSchedulesResponse {
        schedules: vec![schedule(1, ScheduleState::Active { owner: device(1) })],
        incomplete: true,
    });
    present(&mut index, Lang::En, device(1), Vec::new());
    assert_eq!(index.state(), CatalogState::Incomplete);
    assert_eq!(index.rows().len(), 1);

    index.take_error();
    assert_eq!(index.state(), CatalogState::Error);
    // A transient watch failure does not blank the last truthful rows.
    assert_eq!(index.rows().len(), 1);
}

#[test]
fn mutation_subject_keeps_stable_canonical_facts_by_schedule_id() {
    let owner = device(1);
    let view = schedule(1, ScheduleState::Active { owner });
    let id = view.header.schedule;
    let workspace = view.workspace;
    let document = view.document;
    let definition = view.header.definition.clone();
    let mut index = ScheduleIndex::inert();
    index.take_listing(ListSchedulesResponse {
        schedules: vec![view],
        incomplete: false,
    });

    let subject = index.subject(id).unwrap();
    assert_eq!(subject.schedule, id);
    assert_eq!(subject.workspace, workspace);
    assert_eq!(subject.document, document);
    assert_eq!(subject.owner, owner);
    assert_eq!(subject.definition, definition);
    assert_eq!(subject.name.as_str(), "Daily review");
}

#[test]
fn rename_worker_preserves_the_stable_request_and_returns_canonical_view() {
    struct RenameSource {
        request: std::sync::mpsc::SyncSender<RenameScheduleRequest>,
        response: RenameScheduleResponse,
        opened: std::sync::mpsc::SyncSender<()>,
    }
    impl ScheduleSource for RenameSource {
        fn list(&self, _workspace: WorkspaceId) -> Result<ListSchedulesResponse, Failure> {
            unreachable!()
        }

        fn watch(
            &self,
            _workspace: WorkspaceId,
            _handler: WatchHandler,
        ) -> Result<Box<dyn ScheduleWatch>, Failure> {
            self.opened.send(()).unwrap();
            Ok(Box::new(FakeWatch))
        }

        fn rename(
            &self,
            request: &RenameScheduleRequest,
        ) -> Result<RenameScheduleResponse, Failure> {
            self.request.send(request.clone()).unwrap();
            Ok(self.response.clone())
        }
    }

    let owner = device(1);
    let mut view = schedule(1, ScheduleState::Active { owner });
    let expected_name = view.name.clone().unwrap();
    let name = koru_delegate_schedule::ScheduleName::new("Renamed schedule").unwrap();
    view.name = Some(name.clone());
    let request = RenameScheduleRequest {
        schedule: view.header.schedule,
        workspace: view.workspace,
        expected_name,
        name,
    };
    let (asks, inbox) = channel();
    let relay = asks.clone();
    let (outbox, answers) = channel();
    let thread = std::thread::spawn(move || worker(inbox, outbox, relay));
    let (opened, wait_opened) = std::sync::mpsc::sync_channel(1);
    let (sent, request_seen) = std::sync::mpsc::sync_channel(1);
    asks.send(Ask::Source {
        token: 7,
        source: Box::new(RenameSource {
            request: sent,
            response: RenameScheduleResponse {
                schedule: view.clone(),
            },
            opened,
        }),
        workspace: view.workspace,
    })
    .unwrap();
    wait_opened.recv().unwrap();
    asks.send(Ask::Rename {
        token: 7,
        request: request.clone(),
    })
    .unwrap();
    assert_eq!(request_seen.recv().unwrap(), request);
    match answers.recv().unwrap() {
        Answer::Renamed { answer, .. } => {
            assert_eq!(answer.schedule, view.header.schedule);
            assert_eq!(answer.result.unwrap().schedule, view);
        }
        _ => panic!("expected the rename answer"),
    }
    asks.send(Ask::Stop).unwrap();
    thread.join().unwrap();
}

#[test]
fn active_paused_remote_handoff_and_retired_states_project_truthfully() {
    let here = device(1);
    let remote = device(2);
    let mut listings = vec![
        schedule(1, ScheduleState::Active { owner: here }),
        schedule(
            2,
            ScheduleState::Paused {
                owner: remote,
                reason: SchedulePauseReason::ProviderNotDelegatable,
            },
        ),
        schedule(
            3,
            ScheduleState::Paused {
                owner: here,
                reason: SchedulePauseReason::ContextUnavailable {
                    item: 1,
                    stage: ScheduleContextFailureStage::Materialize,
                },
            },
        ),
        schedule(
            4,
            ScheduleState::HandoffPending {
                owner: remote,
                requests: vec![koru_delegate_core::HandoffId::from_uuid(v4(70))],
            },
        ),
        schedule(5, ScheduleState::Retired),
    ];
    listings[1].next_firing = None;
    listings[3].next_firing = None;
    let mut index = ScheduleIndex::inert();
    index.take_listing(ListSchedulesResponse {
        schedules: listings,
        incomplete: false,
    });
    present(&mut index, Lang::En, here, Vec::new());
    assert_eq!(index.rows().len(), 4);
    assert!(
        index
            .rows()
            .iter()
            .any(|row| row.owner.contains("Studio") && row.owner.contains("this device"))
    );
    assert!(index.rows().iter().any(|row| row.owner.contains("Laptop")));
    assert!(
        index
            .rows()
            .iter()
            .any(|row| row.state.contains("agent unavailable"))
    );
    assert!(
        index
            .rows()
            .iter()
            .any(|row| row.state.contains("context item 2"))
    );
    assert!(
        index
            .rows()
            .iter()
            .any(|row| row.state.contains("transfer pending"))
    );
}

#[test]
fn every_paused_reason_and_context_stage_has_specific_readable_state() {
    let owner = device(1);
    let names = HashMap::from([(owner, "Studio".into())]);
    let reasons = [
        SchedulePauseReason::Manual,
        SchedulePauseReason::ProviderNotDelegatable,
        SchedulePauseReason::ScopeUnavailable,
        SchedulePauseReason::ContextUnavailable {
            item: 0,
            stage: ScheduleContextFailureStage::Resolve,
        },
        SchedulePauseReason::ContextUnavailable {
            item: 1,
            stage: ScheduleContextFailureStage::Commit,
        },
        SchedulePauseReason::ContextUnavailable {
            item: 2,
            stage: ScheduleContextFailureStage::Retention,
        },
        SchedulePauseReason::ContextUnavailable {
            item: 3,
            stage: ScheduleContextFailureStage::Materialize,
        },
    ];
    for reason in reasons {
        let (_, words) = state_words(
            &ScheduleState::Paused { owner, reason },
            Lang::En,
            Some(owner),
            &names,
        )
        .unwrap();
        assert!(words.starts_with("State: Paused"), "{reason:?}: {words}");
        assert!(!words.contains("{}"), "{reason:?}: {words}");
    }
}

#[test]
fn unnamed_device_owners_keep_local_and_remote_identity_distinct() {
    let here = device(1);
    let remote = device(2);
    let none = HashMap::new();
    let (local, _) = state_words(
        &ScheduleState::Active { owner: here },
        Lang::En,
        Some(here),
        &none,
    )
    .unwrap();
    let (other, _) = state_words(
        &ScheduleState::Active { owner: remote },
        Lang::En,
        Some(here),
        &none,
    )
    .unwrap();
    assert_eq!(local, "Owner: This device");
    assert_eq!(other, "Owner: Another device");
}

#[test]
fn fired_runs_join_once_and_skips_or_missing_runs_never_become_children() {
    let fired_a = run(30);
    let fired_b = run(31);
    let skipped = run(32);
    let missing = run(33);
    let mut view = schedule(1, ScheduleState::Active { owner: device(1) });
    view.firings = vec![
        ScheduleFiringView {
            occurrence_ms: 1,
            revision: 1,
            outcome: ScheduleFiringOutcome::Fired,
            run: Some(fired_a),
            busy_holder: None,
            at: 1,
        },
        ScheduleFiringView {
            occurrence_ms: 2,
            revision: 1,
            outcome: ScheduleFiringOutcome::SkippedBusy,
            // Defensive wire fixture: a malformed/forward response must not
            // turn a skip into a child merely because it carried a RunId.
            run: Some(skipped),
            busy_holder: Some(skipped),
            at: 2,
        },
        ScheduleFiringView {
            occurrence_ms: 3,
            revision: 1,
            outcome: ScheduleFiringOutcome::Fired,
            run: Some(missing),
            busy_holder: None,
            at: 3,
        },
        ScheduleFiringView {
            occurrence_ms: 4,
            revision: 1,
            outcome: ScheduleFiringOutcome::Fired,
            run: Some(fired_b),
            busy_holder: None,
            at: 4,
        },
    ];
    let mut index = ScheduleIndex::inert();
    index.take_listing(ListSchedulesResponse {
        schedules: vec![view],
        incomplete: false,
    });
    present(
        &mut index,
        Lang::En,
        device(1),
        vec![run_row(fired_a), run_row(skipped), run_row(fired_b)],
    );
    assert_eq!(
        index.rows()[0]
            .runs
            .iter()
            .map(|row| row.run)
            .collect::<Vec<_>>(),
        vec![fired_b, fired_a]
    );
}

#[test]
fn a_new_firing_requests_one_run_refresh_then_joins_the_answer() {
    let here = device(1);
    let fired = run(40);
    let mut view = schedule(1, ScheduleState::Active { owner: here });
    let mut index = ScheduleIndex::inert();
    index.take_listing(ListSchedulesResponse {
        schedules: vec![view.clone()],
        incomplete: false,
    });
    present(&mut index, Lang::En, here, Vec::new());
    assert!(!index.take_runs_refresh());

    view.firings.push(ScheduleFiringView {
        occurrence_ms: 5,
        revision: 1,
        outcome: ScheduleFiringOutcome::Fired,
        run: Some(fired),
        busy_holder: None,
        at: 5,
    });
    index.take_listing(ListSchedulesResponse {
        schedules: vec![view.clone()],
        incomplete: false,
    });
    assert!(
        index.take_runs_refresh(),
        "the new child asks for the run list"
    );
    assert!(
        !index.take_runs_refresh(),
        "the request edge is coalesced until another listing arrives"
    );
    assert!(
        !index.take_runs_refresh(),
        "an idle index does not poll while the request is in flight"
    );
    // If the run-list request failed, a later schedule-list event can re-arm
    // it even when the gateway's recovered listing has identical membership.
    index.take_listing(ListSchedulesResponse {
        schedules: vec![view],
        incomplete: false,
    });
    assert!(
        index.take_runs_refresh(),
        "a later listing retries while the fired child is still unknown"
    );

    present(&mut index, Lang::En, here, vec![run_row(fired)]);
    assert_eq!(index.rows()[0].runs[0].run, fired);
}

#[test]
fn timezone_change_reprojects_new_exact_epoch_without_an_unrelated_trigger() {
    let here = device(1);
    let names = HashMap::from([(here, "Studio".into())]);
    let new_york = TimeZoneName::parse("America/New_York").unwrap();
    let los_angeles = TimeZoneName::parse("America/Los_Angeles").unwrap();
    let mut view = schedule(1, ScheduleState::Active { owner: here });
    // 09:00 local in New York on the same winter day.
    view.next_firing = Some(ScheduleNextFiring::Exact {
        at_ms: 1_767_362_400_000,
    });
    let mut index = ScheduleIndex::inert();
    index.take_listing(ListSchedulesResponse {
        schedules: vec![view.clone()],
        incomplete: false,
    });
    assert!(index.set_presentation(Lang::En, &new_york, Some(here), &names, Some(&[])));
    let ny_rule = index.rows()[0].rule.clone();
    let ny_next = index.rows()[0].next.clone();
    assert!(ny_rule.contains("9:00 AM"), "{ny_rule}");
    assert!(ny_next.contains("9:00 AM"), "{ny_next}");

    // The API has recomputed the exact instant for 09:00 after the device
    // moved to Los Angeles. Language, owner, device names, and runs are the
    // same; the maintenance environment's zone is the only presentation edge.
    view.next_firing = Some(ScheduleNextFiring::Exact {
        at_ms: 1_767_373_200_000,
    });
    index.take_listing(ListSchedulesResponse {
        schedules: vec![view],
        incomplete: false,
    });
    assert!(index.set_presentation(Lang::En, &los_angeles, Some(here), &names, Some(&[])));
    assert_eq!(index.rows()[0].rule, ny_rule);
    assert!(index.rows()[0].next.contains("9:00 AM"));
    assert_eq!(index.zone.zone(), &los_angeles);
}

#[test]
fn every_language_formats_complete_schedule_rows_and_arabic_bounds_values() {
    for lang in Lang::ALL {
        let mut index = ScheduleIndex::inert();
        index.take_listing(ListSchedulesResponse {
            schedules: vec![schedule(1, ScheduleState::Active { owner: device(1) })],
            incomplete: false,
        });
        present(&mut index, lang, device(1), Vec::new());
        let row = &index.rows()[0];
        for value in [&row.rule, &row.next, &row.owner, &row.state] {
            assert!(!value.is_empty(), "{lang:?}");
            assert!(!value.contains("{}"), "{lang:?}: {value}");
        }
        if lang == Lang::Ar {
            assert!(row.next.contains('\u{2068}'));
            assert!(row.owner.contains('\u{2068}'));
        }
    }
}

#[test]
fn watch_bursts_coalesce_without_an_idle_timer() {
    let (send, receive) = channel();
    let pending = Arc::new(AtomicBool::new(false));
    let change = ScheduleChange {
        instance: v4(200),
        generation: 1,
    };
    for _ in 0..100 {
        queue_refresh(&send, 7, Ok(change.clone()), &pending);
    }
    let first = receive.try_recv().unwrap();
    assert!(
        receive.try_recv().is_err(),
        "one watch burst queues one read"
    );
    match first {
        Ask::Invalidated { pending, .. } => pending.store(false, Ordering::Release),
        _ => panic!("watch queued a non-refresh command"),
    }
    queue_refresh(&send, 7, Ok(change), &pending);
    assert!(matches!(receive.try_recv(), Ok(Ask::Invalidated { .. })));
    assert!(receive.try_recv().is_err());
    // There is deliberately no tick/timeout command: without a watch edge the
    // worker blocks on `recv`, and this queue remains empty.
}

#[test]
fn an_ok_behind_an_error_is_still_one_refresh_opportunity() {
    let (send, receive) = channel();
    let pending = Arc::new(AtomicBool::new(false));
    let change = ScheduleChange {
        instance: v4(201),
        generation: 4,
    };
    queue_refresh(
        &send,
        7,
        Err(Failure::new("gateway_unavailable", "gone", true)),
        &pending,
    );
    queue_refresh(&send, 7, Ok(change), &pending);

    assert!(matches!(receive.try_recv(), Ok(Ask::Invalidated { .. })));
    assert!(
        receive.try_recv().is_err(),
        "Err + Ok coalesce as one list, not an Error answer that hides health"
    );
}

#[test]
fn worker_recovers_from_error_when_same_generation_reconnects() {
    let (asks, inbox) = channel();
    let (outbox, answers) = channel();
    let relay = asks.clone();
    let thread = std::thread::spawn(move || worker(inbox, outbox, relay));
    let handler = Arc::new(Mutex::new(None));
    let (opened, wait_opened) = std::sync::mpsc::sync_channel(1);
    let lists = Arc::new(AtomicUsize::new(0));
    let healthy = ListSchedulesResponse {
        schedules: Vec::new(),
        incomplete: false,
    };
    asks.send(Ask::Source {
        token: 9,
        source: Box::new(FakeSource {
            listings: Arc::new(Mutex::new(VecDeque::from([
                Err(Failure::new("gateway_unavailable", "gone", true)),
                Ok(healthy.clone()),
            ]))),
            handler: Arc::clone(&handler),
            opened,
            lists: Arc::clone(&lists),
        }),
        workspace: WorkspaceId::from_uuid(v4(90)),
    })
    .unwrap();
    wait_opened
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    let same_generation = ScheduleChange {
        instance: v4(202),
        generation: 11,
    };

    handler.lock().unwrap().as_mut().unwrap()(Err(Failure::new(
        "gateway_unavailable",
        "stream ended",
        true,
    )));
    assert!(matches!(
        answers
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap(),
        Answer::Error { token: 9 }
    ));

    // The typed watch reports this same cursor when its reconnect handshake
    // succeeds. No schedule mutation/new generation is needed to re-list.
    handler.lock().unwrap().as_mut().unwrap()(Ok(same_generation));
    match answers
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap()
    {
        Answer::Listing { token, listing } => {
            assert_eq!(token, 9);
            assert_eq!(listing, healthy);
        }
        Answer::Error { .. } => panic!("the healthy reconnect stayed in Error"),
        Answer::Renamed { .. } => panic!("the reconnect did not rename a schedule"),
        Answer::Lifecycle { .. } => panic!("the reconnect did not change a schedule"),
        Answer::Retired { .. } => panic!("the reconnect did not retire a schedule"),
    }
    assert_eq!(lists.load(Ordering::Relaxed), 2);
    asks.send(Ask::Stop).unwrap();
    thread.join().unwrap();
}

#[test]
fn workspace_changes_clear_rows_and_repeating_the_same_source_is_free() {
    let mut index = ScheduleIndex::inert();
    index.take_listing(ListSchedulesResponse {
        schedules: vec![schedule(1, ScheduleState::Active { owner: device(1) })],
        incomplete: false,
    });
    present(&mut index, Lang::En, device(1), Vec::new());
    assert_eq!(index.rows().len(), 1);
    let workspace = WorkspaceId::from_uuid(v4(90));
    assert!(index.set_source(None, Some(workspace)));
    assert!(index.rows().is_empty());
    assert_eq!(index.state(), CatalogState::Loading);
    assert!(!index.set_source(None, Some(workspace)));
}
