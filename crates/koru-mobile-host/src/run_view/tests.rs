// SPDX-License-Identifier: MPL-2.0

//! The phone's view of a run, through the host's own path: a `MobileHost`
//! on a temporary root, a run seeded into its embedded store the way the
//! executing device writes one (`RunDocument` + `apply_update` + `flush`),
//! and the two reads the phone performs — `view_run` and
//! `run_timeline_window`.
//!
//! The criterion of P1.0-STORAGE-PROPOSAL.md §9 is held as **ratios and
//! counters, never wall-clock budgets**: forks and store round trips are
//! counted, and opening a long run is compared with opening a short one and
//! with opening a page in the same process. The figures themselves are the
//! ignored test at the end:
//!
//! ```sh
//! cargo test --release -p koru-mobile-host run_view_figures -- --ignored --nocapture
//! ```

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use koru_delegate_core::{
    ActorRef, AgentTurnId, AmendmentRevision, ArtifactTitle, ArtifactVersionId, Attention,
    AttentionSet, Completeness, ControlState, DelegationRequest, FailureStage, ModelRef, Objective,
    PriorAttempt, Rating, RatingNote, ResultReference, RunAttemptId, RunHeader, RunId, RunRating,
    RunStatus, SessionProfileKey, StepKind, TimelineEntry, TimelineRef, TimelineRefs,
    TurnOriginator, UsageFact, UsageFactId, WorkspaceScope,
};
use koru_delegate_document::{RUN_KIND, RunDocument, SCHEDULE_KIND};
use koru_store_core::ports::UpdateOrigin;
use koru_store_core::{DocumentId, Timestamp, WorkspaceId};
use loro::VersionVector;

use crate::run_view::{MAX_TIMELINE_WINDOW, RUN_PILL_ROLE, TIMELINE_WINDOW};
use crate::{DeviceId, MobileHost, ViewReference};

fn at(ms: i64) -> Timestamp {
    Timestamp::from_millis(1_770_000_000_000 + ms)
}

fn host() -> (tempfile::TempDir, Arc<MobileHost>) {
    let root = tempfile::tempdir().unwrap();
    let host = MobileHost::open(root.path().to_path_buf(), [71; 64], "Test", false).unwrap();
    (root, host)
}

fn with_store<R, F>(host: &MobileHost, task: F) -> R
where
    R: Send + 'static,
    F: FnOnce(&mut koru_store_server::WorkerStore) -> R + Send + 'static,
{
    host.store.with_store(task).unwrap()
}

fn a_workspace(host: &MobileHost) -> WorkspaceId {
    with_store(host, |store| {
        let profile = store
            .create_profile(
                "Owner",
                None,
                None,
                koru_store_core::Locale::parse("en").unwrap(),
                at(0),
            )
            .unwrap();
        store
            .create_workspace(profile.id, "Workspace", at(0))
            .unwrap()
            .id
    })
}

/// This phone's own device id, as its trust catalog names it.
fn local_device(host: &MobileHost) -> DeviceId {
    host.trust
        .lock()
        .unwrap()
        .as_ref()
        .expect("a fresh host has an identity")
        .view()
        .expect("a fresh host's trust view reads")
        .local_device
        .device_id
}

fn a_header(device: DeviceId) -> RunHeader {
    RunHeader::from_request(
        DelegationRequest {
            run: RunId::mint(),
            requested_by: ActorRef::Person {
                profile: koru_store_core::ProfileId::new(),
            },
            objective: Objective::new("tidy the release notes\nsecond line").unwrap(),
            agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
            requested_model: None,
            scope: WorkspaceScope::Temporary,
            context: Vec::new(),
            grants: BTreeSet::new(),
            executing_device: device,
            origin: Default::default(),
        },
        1_770_000_000_000,
    )
}

fn a_turn() -> AgentTurnId {
    DocumentId::new().to_storage().parse().unwrap()
}

/// P1.0's six-step cycle in the vocabulary — the mix the proposal measured
/// its figures against, so the figures here compare with `run_document.rs`.
fn append_cycle(run: &RunDocument, attempt: RunAttemptId, turn: &mut AgentTurnId, i: usize) {
    let at = 1_770_000_000_000 + i as i64 * 1_500;
    match i % 6 {
        0 => {
            *turn = a_turn();
            run.append_step(&TimelineEntry::new(
                StepKind::TurnStarted {
                    originator: TurnOriginator::Orchestrator,
                },
                TimelineRefs::new([TimelineRef::Turn { turn: *turn }]).unwrap(),
                at,
            ))
            .unwrap();
        }
        1..=3 => run
            .append_step(&TimelineEntry::new(
                StepKind::ToolsCompleted {
                    calls: (i % 5) as u32 + 1,
                    failed: 0,
                    failures: Default::default(),
                },
                TimelineRefs::new([TimelineRef::Document {
                    document: DocumentId::new(),
                    version: koru_delegate_core::DocumentVersionId::new(),
                }])
                .unwrap(),
                at,
            ))
            .unwrap(),
        4 => run
            .append_step(&TimelineEntry::new(
                StepKind::TurnCompleted {
                    originator: TurnOriginator::Orchestrator,
                    artifacts_registered: 0,
                },
                TimelineRefs::new([TimelineRef::Turn { turn: *turn }]).unwrap(),
                at,
            ))
            .unwrap(),
        _ => run
            .record_usage(&UsageFact {
                identity: UsageFactId::mint(),
                attempt,
                turn: *turn,
                revision: AmendmentRevision(0),
                input: Some((i % 900) as u64),
                output: Some((i % 300) as u64),
                cached_input: None,
                model: ModelRef::from_uuid("00000000-0000-4000-8000-000000000001".parse().unwrap()),
                completeness: Completeness::Complete,
            })
            .unwrap(),
    }
}

/// A run in memory with `steps` entries, plus whatever `shape` adds.
fn a_run(header: &RunHeader, steps: usize, shape: impl FnOnce(&RunDocument)) -> RunDocument {
    let run = RunDocument::create(header).unwrap();
    let attempt = RunAttemptId::mint();
    let mut turn = a_turn();
    for i in 0..steps {
        append_cycle(&run, attempt, &mut turn, i);
    }
    shape(&run);
    run
}

/// Register a run in the store and commit `run`'s whole history to it in
/// one update — the bytes are the same the executing device would have sent
/// a step at a time, and a fork costs the same whichever way they arrived.
fn seed_run(host: &MobileHost, workspace: WorkspaceId, run: &RunDocument) -> DocumentId {
    let bytes = run.export_updates_since(&VersionVector::default()).unwrap();
    with_store(host, move |store| {
        let (id, _) = store
            .create_document(workspace, "Run", RUN_KIND, None, None, at(1))
            .unwrap();
        store
            .apply_update(workspace, id, bytes, at(2), UpdateOrigin::Store)
            .unwrap();
        store.flush(workspace, id, at(2)).unwrap();
        id
    })
}

/// Send `run`'s history since `since` to the store, as a later step would.
fn send_since(
    host: &MobileHost,
    workspace: WorkspaceId,
    id: DocumentId,
    run: &RunDocument,
    since: &VersionVector,
    at_ms: i64,
) {
    let bytes = run.export_updates_since(since).unwrap();
    with_store(host, move |store| {
        store
            .apply_update(workspace, id, bytes, at(at_ms), UpdateOrigin::Store)
            .unwrap();
        store.flush(workspace, id, at(at_ms)).unwrap();
    });
}

/// A page of `paragraphs` paragraphs, seeded the way the host seeds a note.
fn seed_page(host: &MobileHost, workspace: WorkspaceId, paragraphs: usize) -> DocumentId {
    with_store(host, move |store| {
        let (id, _) = store
            .create_document(workspace, "Page", "page", None, None, at(1))
            .unwrap();
        let snapshot = store.snapshot(workspace, id).unwrap();
        let mut model = koru_core::WorkspaceDoc::from_snapshot(&snapshot).unwrap();
        let before = model.version();
        let root = model
            .insert(
                koru_core::schema::Parent::Root,
                koru_core::Node::document("Page").unwrap(),
            )
            .unwrap();
        for i in 0..paragraphs {
            model
                .insert(
                    koru_core::schema::Parent::Node(root),
                    koru_core::Node::paragraph(format!("Paragraph {i} of the page")).unwrap(),
                )
                .unwrap();
        }
        model.stamp_schema_version().unwrap();
        let update = model.export_updates_since(&before).unwrap();
        store
            .apply_update(workspace, id, update, at(2), UpdateOrigin::Store)
            .unwrap();
        store.flush(workspace, id, at(2)).unwrap();
        id
    })
}

fn a_result(device: DeviceId) -> ResultReference {
    ResultReference {
        artifact: ArtifactVersionId::mint(),
        held_by: device,
        title: ArtifactTitle::new("notes.md").unwrap(),
        size_bytes: 2_048,
        registered_at_ms: 1_770_000_000_500,
    }
}

fn forks(host: &MobileHost) -> u64 {
    host.run_forks.load(Ordering::Relaxed)
}

fn store_calls(host: &MobileHost) -> u64 {
    host.store_calls.load(Ordering::Relaxed)
}

fn forget_open_runs(host: &MobileHost) {
    host.run_views.lock().unwrap().clear();
}

// ---------------------------------------------------------------- header

/// The header the Overview shows: status with its markers, the agent, the
/// objective, the scope, the result named with the device that holds it
/// (D27), the economics as numbers, and the rating — read from a run
/// executed by *another* device, which is the phone's usual case (D15).
#[test]
fn a_run_opens_with_the_header_fields_the_overview_shows() {
    let (_root, host) = host();
    let ws = a_workspace(&host);
    let desktop = DeviceId::new();
    let mut header = a_header(desktop);
    header.status = RunStatus::Running {
        attention: AttentionSet::new([Attention::ApprovalRequired]),
        control: ControlState::Human {
            objective_satisfied: false,
        },
    };
    let run = a_run(&header, 12, |run| {
        run.register_result(&a_result(desktop)).unwrap();
        run.set_rating(&RunRating {
            rating: Rating::Good,
            note: Some(RatingNote::new("good enough").unwrap()),
            rated_by: header.requested_by,
            rated_at_ms: 9_000,
        })
        .unwrap();
        run.refresh_economics(1_770_000_100_000).unwrap();
    });
    let id = seed_run(&host, ws, &run);

    let view = host.view_run(&id.to_storage()).unwrap();
    assert_eq!(view.document_id, id.to_storage());
    assert_eq!(view.workspace_id, ws.to_storage());
    assert_eq!(view.run_id, header.run.to_storage());
    assert_eq!(view.status, "running");
    assert_eq!(view.attention, vec!["approval_required".to_owned()]);
    assert_eq!(view.control.as_deref(), Some("human"));
    assert_eq!(view.agent, "codex");
    assert_eq!(view.objective, "tidy the release notes\nsecond line");
    assert_eq!(view.scope_kind, "temporary");
    assert_eq!(view.context_count, 0);
    assert_eq!(view.created_at_millis, 1_770_000_000_000);
    assert_eq!(view.timeline_len, 12);
    assert!(!view.needs_newer_koru);

    // D16: the executing device is named from this phone's catalog. A
    // device this phone has never met has an id and no name — and it is
    // not this phone.
    assert_eq!(view.executing_device_id, desktop.to_storage());
    assert_eq!(view.executing_device_name, None);
    assert!(!view.executing_here);

    // D27: the result is named, with the device that holds it.
    let result = view.results.first().expect("one result");
    assert_eq!(result.title, "notes.md");
    assert_eq!(result.size_bytes, 2_048);
    assert_eq!(result.held_by_device_id, desktop.to_storage());
    assert!(!result.held_here);

    // The economics summary as numbers: two usage facts in twelve steps,
    // and a count nobody reported stays unavailable rather than zero.
    assert_eq!(view.economics.turns_with_usage, 2);
    assert_eq!(view.economics.orchestrator_turns, 2);
    assert_eq!(view.economics.tool_calls, 2 + 3 + 4 + 3 + 4 + 5);
    assert_eq!(view.economics.input_tokens, Some(5 + 11));
    assert_eq!(view.economics.cached_input_tokens, None);
    assert_eq!(view.economics.cached_input_unavailable_turns, 2);
    assert_eq!(view.economics.models, 1);
    assert!(view.economics.complete);

    // D44: the rating, read-only on the phone.
    assert_eq!(view.rating.as_deref(), Some("good"));
    assert_eq!(view.rating_note.as_deref(), Some("good enough"));
    assert_eq!(view.rated_at_millis, Some(9_000));
}

/// A run this phone executes says so, and names itself from its own
/// catalog — the one case D16 does not name the device elsewhere.
#[test]
fn a_run_executed_by_this_phone_is_named_as_here() {
    let (_root, host) = host();
    let ws = a_workspace(&host);
    let me = local_device(&host);
    let run = a_run(&a_header(me), 3, |run| {
        run.register_result(&a_result(me)).unwrap();
    });
    let id = seed_run(&host, ws, &run);

    let view = host.view_run(&id.to_storage()).unwrap();
    assert!(view.executing_here);
    assert_eq!(view.executing_device_name.as_deref(), Some("Test"));
    assert!(view.results[0].held_here);
    assert_eq!(view.results[0].held_by_device_name.as_deref(), Some("Test"));
    assert_eq!(view.rating, None, "an unrated run is a normal state");
}

/// Every status the phone can meet crosses as the vocabulary's own token,
/// with its stage or its prior-attempt answer beside it.
#[test]
fn every_status_crosses_as_its_own_token() {
    let (_root, host) = host();
    let ws = a_workspace(&host);
    let cases = [
        (RunStatus::Created, "created", None, None),
        (
            RunStatus::Interrupted {
                prior_attempt: PriorAttempt::Unknown,
            },
            "interrupted",
            None,
            Some("unknown"),
        ),
        (RunStatus::Completed, "completed", None, None),
        (
            RunStatus::Failed {
                stage: FailureStage::ProviderStart,
            },
            "failed",
            Some("provider_start"),
            None,
        ),
        (RunStatus::Cancelled, "cancelled", None, None),
    ];
    for (status, token, stage, prior) in cases {
        let mut header = a_header(DeviceId::new());
        header.status = status;
        let id = seed_run(&host, ws, &a_run(&header, 0, |_| {}));
        let view = host.view_run(&id.to_storage()).unwrap();
        assert_eq!(view.status, token);
        assert_eq!(view.failure_stage.as_deref(), stage, "{token}");
        assert_eq!(view.prior_attempt.as_deref(), prior, "{token}");
        assert!(view.attention.is_empty());
        assert_eq!(view.control, None);
    }
}

// -------------------------------------------------------------- windows

/// **Criteria 2 and 3.** The first fifty entries are read from the fork
/// the open took, and scrolling back ten windows — whatever the offset —
/// takes no further fork and no further store round trip.
#[test]
fn windows_read_by_offset_newest_first_from_one_fork_and_no_store_read() {
    let (_root, host) = host();
    let ws = a_workspace(&host);
    let steps = 1_000;
    let id = seed_run(&host, ws, &a_run(&a_header(DeviceId::new()), steps, |_| {}));
    let key = id.to_storage();

    let view = host.view_run(&key).unwrap();
    assert_eq!(view.timeline_len, steps as u32);
    assert_eq!(forks(&host), 1, "one open, one fork");
    let calls_after_open = store_calls(&host);

    // The first window: the newest fifty, newest first.
    let first = host.run_timeline_window(&key, 0, TIMELINE_WINDOW).unwrap();
    assert_eq!(first.rows.len(), TIMELINE_WINDOW);
    assert_eq!(first.timeline_len, steps as u32);
    assert_eq!(first.revision, view.revision);
    let times: Vec<i64> = first.rows.iter().filter_map(|row| row.at_millis).collect();
    assert!(
        times.windows(2).all(|pair| pair[0] >= pair[1]),
        "newest first"
    );
    assert_eq!(times[0], 1_770_000_000_000 + (steps as i64 - 1) * 1_500);
    // The newest entry of a thousand-step cycle (i = 999, 999 % 6 == 3) is a
    // coalesced tool batch with its one document reference (two ids).
    let newest = &first.rows[0];
    assert_eq!(newest.kind, "step");
    assert_eq!(newest.step.as_deref(), Some("tools_completed"));
    assert_eq!(newest.calls, Some(5));
    assert_eq!(newest.refs, 1);
    // A usage fact rides the same list with its numbers.
    let usage = first.rows.iter().find(|row| row.kind == "usage").unwrap();
    assert!(usage.input_tokens.is_some());
    assert_eq!(usage.completeness.as_deref(), Some("complete"));

    // Ten windows back: each fifty rows, none a store read, none a fork.
    for page in 1..=10 {
        let window = host
            .run_timeline_window(&key, page * TIMELINE_WINDOW, TIMELINE_WINDOW)
            .unwrap();
        assert_eq!(window.rows.len(), TIMELINE_WINDOW, "window {page}");
        assert_eq!(window.newest_first_from as usize, page * TIMELINE_WINDOW);
    }
    assert_eq!(forks(&host), 1, "ten windows, still one fork");
    assert_eq!(
        store_calls(&host),
        calls_after_open,
        "a window is not a store read"
    );

    // The oldest window is short, and past the end is empty.
    assert_eq!(
        host.run_timeline_window(&key, steps - 20, TIMELINE_WINDOW)
            .unwrap()
            .rows
            .len(),
        20
    );
    assert!(
        host.run_timeline_window(&key, steps, TIMELINE_WINDOW)
            .unwrap()
            .rows
            .is_empty()
    );
    // Zero asks for the default window; a request past the bound is bounded.
    assert_eq!(
        host.run_timeline_window(&key, 0, 0).unwrap().rows.len(),
        TIMELINE_WINDOW
    );
    assert_eq!(
        host.run_timeline_window(&key, 0, 10_000)
            .unwrap()
            .rows
            .len(),
        MAX_TIMELINE_WINDOW
    );
}

/// The observer's re-read: an unchanged version reuses the fork; a step
/// appended on the executing device moves the revision and costs exactly
/// one more fork, after which windows read the new entry first.
#[test]
fn a_repeated_open_reuses_the_fork_and_a_new_step_costs_one_more() {
    let (_root, host) = host();
    let ws = a_workspace(&host);
    let header = a_header(DeviceId::new());
    let run = a_run(&header, 60, |_| {});
    let id = seed_run(&host, ws, &run);
    let key = id.to_storage();

    let first = host.view_run(&key).unwrap();
    let again = host.view_run(&key).unwrap();
    assert_eq!(again, first);
    assert_eq!(forks(&host), 1, "the same version is the same fork");

    let since = run.version();
    run.append_step(&TimelineEntry::new(
        StepKind::Completed,
        TimelineRefs::default(),
        1_770_000_999_000,
    ))
    .unwrap();
    run.set_status(&RunStatus::Completed).unwrap();
    send_since(&host, ws, id, &run, &since, 500);

    let moved = host.view_run(&key).unwrap();
    assert_ne!(moved.revision, first.revision);
    assert_eq!(moved.status, "completed");
    assert_eq!(moved.timeline_len, 61);
    assert_eq!(forks(&host), 2);
    let window = host.run_timeline_window(&key, 0, TIMELINE_WINDOW).unwrap();
    assert_eq!(window.rows[0].step.as_deref(), Some("completed"));
    assert_eq!(window.revision, moved.revision);
}

/// A window asked for before the run was opened opens it — one fork — so
/// the Activity list never reads a run the observer has not yet reported.
#[test]
fn a_window_before_an_open_opens_the_run_once() {
    let (_root, host) = host();
    let ws = a_workspace(&host);
    let id = seed_run(&host, ws, &a_run(&a_header(DeviceId::new()), 5, |_| {}));
    let window = host.run_timeline_window(&id.to_storage(), 0, 3).unwrap();
    assert_eq!(window.rows.len(), 3);
    assert_eq!(forks(&host), 1);
    host.run_timeline_window(&id.to_storage(), 3, 3).unwrap();
    assert_eq!(forks(&host), 1);
}

/// An entry a newer build wrote keeps its place as a gap rather than
/// making the window one step shorter.
#[test]
fn an_entry_this_build_cannot_decode_is_a_gap_in_its_place() {
    let (_root, host) = host();
    let ws = a_workspace(&host);
    let run = a_run(&a_header(DeviceId::new()), 2, |run| {
        run.doc()
            .get_list("timeline")
            .push(loro::LoroValue::from(serde_json::json!({
                "entry": "future_shape",
                "id": "00000000-0000-4000-8000-0000000000ff",
            })))
            .unwrap();
        run.doc().commit();
    });
    let id = seed_run(&host, ws, &run);
    let window = host.run_timeline_window(&id.to_storage(), 0, 10).unwrap();
    assert_eq!(window.rows.len(), 3);
    assert_eq!(window.rows[0].kind, "gap");
    assert_eq!(window.rows[0].step, None);
    assert_eq!(window.rows[1].kind, "step");
    assert_eq!(host.view_run(&id.to_storage()).unwrap().timeline_len, 3);
}

// ------------------------------------------------------------ the gate

/// A run written by a newer build opens: what this build decodes is shown,
/// and the view says a newer Koru is needed — never a crash, never a
/// refusal of the whole run.
#[test]
fn a_run_from_a_newer_build_says_it_needs_a_newer_koru() {
    let (_root, host) = host();
    let ws = a_workspace(&host);
    let run = a_run(&a_header(DeviceId::new()), 4, |run| {
        run.doc()
            .get_map("run")
            .insert(
                "schema",
                koru_delegate_document::RUN_DOCUMENT_SCHEMA_VERSION + 1,
            )
            .unwrap();
        run.doc().commit();
    });
    let id = seed_run(&host, ws, &run);
    let view = host.view_run(&id.to_storage()).unwrap();
    assert!(view.needs_newer_koru);
    assert_eq!(view.status, "created");
    assert_eq!(view.timeline_len, 4);
    assert_eq!(
        host.run_timeline_window(&id.to_storage(), 0, 2)
            .unwrap()
            .rows
            .len(),
        2
    );
}

/// The wrong door names itself: a page is not a run, a run is not a page,
/// and neither is walked as the other.
#[test]
fn a_page_is_not_a_run_and_a_run_is_not_a_page() {
    let (_root, host) = host();
    let ws = a_workspace(&host);
    let page = seed_page(&host, ws, 3);
    let run = seed_run(&host, ws, &a_run(&a_header(DeviceId::new()), 1, |_| {}));
    assert_eq!(
        host.view_run(&page.to_storage()).unwrap_err(),
        "document_is_not_a_run"
    );
    assert_eq!(
        host.view_document(&run.to_storage()).err().as_deref(),
        Some("document_is_run")
    );
    assert_eq!(
        host.view_run("not-an-id").unwrap_err(),
        "invalid_document_id"
    );
    assert_eq!(
        host.view_run(&DocumentId::new().to_storage()).unwrap_err(),
        "document_unlocated"
    );
    assert_eq!(forks(&host), 0, "a refusal forks nothing");
}

// ---------------------------------------------------- inventory and pill

/// **F6.** The listing a person browses hides runs and schedules — they are
/// reached through the pill — and counts what it hid.
#[test]
fn the_inventory_hides_runs_and_schedules_and_counts_them() {
    let (_root, host) = host();
    let ws = a_workspace(&host);
    let page = seed_page(&host, ws, 1);
    seed_run(&host, ws, &a_run(&a_header(DeviceId::new()), 1, |_| {}));
    with_store(&host, move |store| {
        store
            .create_document(ws, "Schedule", SCHEDULE_KIND, None, None, at(3))
            .unwrap();
    });
    let inventory = host.inventory().unwrap();
    let listed: Vec<String> = inventory
        .documents
        .iter()
        .map(|row| row.id.clone())
        .collect();
    assert_eq!(listed, vec![page.to_storage()]);
    assert_eq!(inventory.hidden_runs, 2);
}

/// **The phone lists by the desktop's rule** (`koru_core::explore::listed`,
/// OCR TECHNICAL §12). The hand-written filter it replaced named notes, runs
/// and schedules and nothing else, so the workspace's system document and
/// its tasks document went out as rows — and a reading document per file
/// would have joined them. Now none of the three is a row, none is counted
/// among the documents the workspace card shows, and none opens; only runs
/// and schedules are counted as hidden, as before.
#[test]
fn the_inventory_lists_by_the_shared_rule_and_hides_internal_documents() {
    let (_root, host) = host();
    let ws = a_workspace(&host);
    let page = seed_page(&host, ws, 1);
    seed_run(&host, ws, &a_run(&a_header(DeviceId::new()), 1, |_| {}));
    let (system, tasks, reading) = with_store(&host, move |store| {
        let (system, _) = store
            .create_document(ws, "", koru_core::explore::KIND_SYSTEM, None, None, at(3))
            .unwrap();
        let (tasks, _) = store
            .create_document(
                ws,
                "Tasks",
                koru_core::explore::KIND_TASK_LIST,
                None,
                None,
                at(3),
            )
            .unwrap();
        let reading = store.ensure_reading(ws, page, at(4)).unwrap().id;
        (system, tasks, reading)
    });
    assert_eq!(
        reading,
        DocumentId::from_uuid(
            koru_core::ids::reading_document_id(koru_core::DocumentId::from_uuid(page.as_uuid()))
                .as_uuid()
        ),
        "the store and the app derive the same id"
    );

    let inventory = host.inventory().unwrap();
    let listed: Vec<String> = inventory
        .documents
        .iter()
        .map(|row| row.id.clone())
        .collect();
    assert_eq!(listed, vec![page.to_storage()], "one row: the page");
    assert_eq!(inventory.hidden_runs, 1, "the run, and only the run");

    for hidden in [system, tasks, reading] {
        assert_eq!(
            host.view_document(&hidden.to_storage()).err().as_deref(),
            Some("document_not_listed"),
        );
    }
    host.view_document(&page.to_storage())
        .expect("the page still opens");
}

/// **Proposal §3.2–3.3.** The pill P1.15 writes — a document reference under
/// the `koru.run` role — reads back through `koru-core` as the typed
/// `AttrValue::Document` now that the reader's normalization covers that role,
/// and as a `Ref` homed at the run on any build whose reader does not. The
/// phone's viewer maps **both** to a run reference carrying the Run document's
/// id (`view_reference_maps_the_run_role_by_its_home` is the other half), so a
/// phone reading a document written or last read by either build shows the
/// same pill. A `document`-role pill beside it still maps to a document.
#[test]
fn a_run_pill_maps_to_a_run_reference_and_a_document_pill_to_a_document() {
    use koru_core::schema::Parent;
    use koru_core::{Attr, AttrValue, DocRef, DocumentReference, Node, Ref};

    let run = koru_core::DocumentId::new();
    let page = koru_core::DocumentId::new();
    let mut model =
        koru_core::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "Pills").unwrap();
    let root = model
        .insert(Parent::Root, Node::document("Carrying page").unwrap())
        .unwrap();
    let paragraph = model
        .insert(
            Parent::Node(root),
            Node::paragraph("A run  and a page ").unwrap(),
        )
        .unwrap();
    model
        .insert_pill(
            paragraph,
            6,
            &Attr::labelled(
                RUN_PILL_ROLE,
                AttrValue::Document(DocumentReference::new(run)),
                "Tidy the notes",
            ),
        )
        .unwrap();
    model
        .insert_pill(
            paragraph,
            18,
            &Attr::labelled(
                "document",
                AttrValue::Ref(Ref::in_doc(
                    koru_core::NodeId::new(),
                    DocRef::Document(page),
                )),
                "A page",
            ),
        )
        .unwrap();

    let view = crate::document_view(
        &model,
        WorkspaceId::new(),
        DocumentId::new(),
        "Carrying page".to_owned(),
        model.version().encode(),
        &std::collections::HashMap::new(),
    )
    .unwrap();
    let inline = &view.blocks[0].rich_text.as_ref().unwrap().inline_objects;
    assert_eq!(inline.len(), 2);
    assert_eq!(inline[0].role, RUN_PILL_ROLE);
    assert_eq!(inline[0].label.as_deref(), Some("Tidy the notes"));
    assert!(matches!(
        &inline[0].reference,
        ViewReference::Run { document_id } if *document_id == run.to_storage()
    ));
    // The reader normalizes the `document` role to a typed reference that
    // keeps its node (F2), which is the document either way.
    assert!(matches!(
        &inline[1].reference,
        ViewReference::Document { document_id, .. } if *document_id == page.to_storage()
    ));
}

/// The same rule on the two spellings the reader may hand back: a `Ref`
/// homed at the run (today's reader) and a typed document reference (the
/// reader P1.15 extends).
#[test]
fn view_reference_maps_the_run_role_by_its_home() {
    use koru_core::{DocRef, Ref};
    let run = koru_core::DocumentId::new();
    let root = koru_core::NodeId::new();
    let reference = Ref::in_doc(koru_core::NodeId::new(), DocRef::Document(run));
    assert!(matches!(
        crate::view_reference(reference, RUN_PILL_ROLE, root, DocumentId::new()),
        ViewReference::Run { document_id } if document_id == run.to_storage()
    ));
    // Any other role is what it was.
    assert!(matches!(
        crate::view_reference(reference, "task", root, DocumentId::new()),
        ViewReference::Block { .. }
    ));
}

// ------------------------------------------------------------ the ratio

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

fn timed(rounds: usize, mut f: impl FnMut()) -> Duration {
    median(
        (0..rounds)
            .map(|_| {
                let started = Instant::now();
                f();
                started.elapsed()
            })
            .collect(),
    )
}

fn once(f: impl FnOnce()) -> Duration {
    let started = Instant::now();
    f();
    started.elapsed()
}

/// **Criterion 1**, as ratios in one process (PRINCIPLES, *measure ratios,
/// not wall clocks*): opening a 10 000-step run costs at most 20× opening a
/// 1 000-step run, and opening a 1 000-step run costs no more than opening a
/// 1 000-paragraph page through the same host — the run is not a special
/// document to the store. Each open is a cold open of the host's cache;
/// the store is warm, as it is on a phone that has synced.
///
/// **How it is timed is what keeps it a ratio of the code on a loaded
/// machine**, which a full `cargo test --workspace` is. It used to take the
/// median of seven back-to-back opens of each kind, and failed there at
/// 23× (86 ms against 3.7 ms) while measuring 12-15× alone, for two reasons
/// the code had nothing to do with:
///
/// - **A short open can dodge the load and a long one cannot.** A 4 ms open
///   often runs inside one scheduling slice on a quiet core; a 50 ms one is
///   always preempted and always shares its core. So the 20× bound is held
///   as one long open against *ten* short ones — 2× — which take about as
///   long and meet the same load.
/// - **The series were timed at different moments.** The three kinds now
///   take turns within every round, so a busy second lands on all of them,
///   and the fastest of each is kept: the open with the least added to it.
///
/// Each measured open follows an unmeasured one of the same document,
/// because the premise is a warm store and an open straight after another
/// document's is not one: a 1 000-step open measured right after a
/// 10 000-step one cost five times its warm figure.
///
/// What remains is real and is the load's, not the code's: the 10 000-step
/// open takes about 12 000 fresh demand-zero pages each time (its large
/// buffers go back to the OS between opens; the 1 000-step one reuses the
/// heap), a quarter of its time alone, and a machine too busy to zero pages
/// in the background makes each fault dearer. Alone the ratio is about 1.4
/// of the 2 allowed; with the whole suite beside 40 CPU burners on 32
/// threads it stayed under 2, and at 48 it touched 2 once in twenty.
#[test]
fn opening_a_run_is_no_worse_than_linear_and_no_dearer_than_a_page() {
    let (_root, host) = host();
    let ws = a_workspace(&host);
    let short = seed_run(&host, ws, &a_run(&a_header(DeviceId::new()), 1_000, |_| {}));
    let long = seed_run(
        &host,
        ws,
        &a_run(&a_header(DeviceId::new()), 10_000, |_| {}),
    );
    let page = seed_page(&host, ws, 1_000);
    let rounds = 15;
    const SHORTS: u32 = 10;

    let open_run = |id: DocumentId| {
        forget_open_runs(&host);
        let view = host.view_run(&id.to_storage()).unwrap();
        host.run_timeline_window(&id.to_storage(), 0, TIMELINE_WINDOW)
            .unwrap();
        view.timeline_len
    };
    let open_page = |id: DocumentId| {
        host.document_views.lock().unwrap().clear();
        host.view_document(&id.to_storage()).unwrap().blocks.len()
    };

    let (mut t_short, mut t_shorts, mut t_long, mut t_page) =
        (Duration::MAX, Duration::MAX, Duration::MAX, Duration::MAX);
    for _ in 0..rounds {
        open_run(short);
        t_short = t_short.min(once(|| {
            open_run(short);
        }));
        t_shorts = t_shorts.min(once(|| {
            for _ in 0..SHORTS {
                open_run(short);
            }
        }));
        open_run(long);
        t_long = t_long.min(once(|| {
            open_run(long);
        }));
        open_page(page);
        t_page = t_page.min(once(|| {
            open_page(page);
        }));
    }
    assert!(
        t_long <= t_shorts * 2,
        "10 000 steps opened in {t_long:?} against {SHORTS} opens of 1 000 in {t_shorts:?}: \
         more than 20x one"
    );
    assert!(
        t_short <= t_page,
        "a 1 000-step run opened in {t_short:?}, dearer than a 1 000-paragraph page in {t_page:?}"
    );
}

/// The figures behind the ratio, on this machine, for the report and for
/// the phone to be compared against with the same shape.
#[ignore = "prints figures; run with --nocapture"]
#[test]
fn run_view_figures() {
    let (_root, host) = host();
    let ws = a_workspace(&host);
    let rounds = 7;
    println!(
        "steps | snapshot bytes | open (fork+header+first window) | ten windows back | forks so far | store round trips per open"
    );
    for steps in [1_000usize, 10_000] {
        let run = a_run(&a_header(DeviceId::new()), steps, |_| {});
        let bytes = run.export_snapshot().unwrap().len();
        let id = seed_run(&host, ws, &run);
        let key = id.to_storage();
        forget_open_runs(&host);
        host.view_run(&key).unwrap();
        let open = timed(rounds, || {
            forget_open_runs(&host);
            host.view_run(&key).unwrap();
            host.run_timeline_window(&key, 0, TIMELINE_WINDOW).unwrap();
        });
        let before = store_calls(&host);
        forget_open_runs(&host);
        host.view_run(&key).unwrap();
        let per_open = store_calls(&host) - before;
        let windows = timed(rounds, || {
            for page in 1..=10 {
                host.run_timeline_window(&key, page * TIMELINE_WINDOW, TIMELINE_WINDOW)
                    .unwrap();
            }
        });
        println!(
            "{steps:>6} | {bytes:>14} | {:>10.3} ms | {:>10.3} ms | {} | {per_open}",
            open.as_secs_f64() * 1e3,
            windows.as_secs_f64() * 1e3,
            forks(&host),
        );
    }
    let page = seed_page(&host, ws, 1_000);
    host.view_document(&page.to_storage()).unwrap();
    let open_page = timed(rounds, || {
        host.document_views.lock().unwrap().clear();
        host.view_document(&page.to_storage()).unwrap();
    });
    println!(
        "a 1 000-paragraph page opens (fork + walk) in {:.3} ms",
        open_page.as_secs_f64() * 1e3
    );
}
