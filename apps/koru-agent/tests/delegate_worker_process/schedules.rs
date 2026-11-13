// SPDX-License-Identifier: MPL-2.0

//! P2.2-P2.4 through the product composition: two rooted Agents, their real
//! Store/gateway/session/delegate workers, and the checked-in Codex fixture.

use std::io::{Seek as _, Write as _};
use std::sync::mpsc;
use std::time::Duration;

use koru_api_client::Connection;
use koru_delegate_api::wire::RunStreamItem;
use koru_delegate_core::{
    ActorRef, DeviceId, Objective, ProfileId, RunContextItem, RunId, RunOrigin, RunStatus,
    SessionProfileKey, WorkspaceScope,
};
use koru_delegate_document::{RUN_KIND, RunDocument, SCHEDULE_KIND, ScheduleDocument};
use koru_delegate_schedule::{
    DelegationTemplate, ScheduleContextFailureStage, ScheduleContextPointer, ScheduleDefinition,
    ScheduleFiringOutcome, ScheduleHeader, ScheduleLifecycle, SchedulePauseReason, ScheduleState,
};
use koru_recurrence::{CronExpression, RecurrenceRule};
use koru_store_core::ImportMode;
use koru_store_core::manifest::Manifest;
use koru_store_core::model::Timestamp;

use super::{Agent, CAPTURED_BODY, EDITED_BODY, a_pinned_page, edit_page};

const BASE_MINUTE: i64 = 1_767_225_600_000;
const OCCURRENCE_STEP: i64 = 3_600_000;
const PROCESS_DEADLINE: Duration = Duration::from_secs(90);

fn scheduled_agent(now_ms: i64) -> Agent {
    let root = tempfile::tempdir().expect("a schedule fixture root");
    std::fs::write(
        root.path().join("delegate-process-test-clock"),
        now_ms.to_string(),
    )
    .expect("seed the fixture clock");
    Agent::with_root(Some("complete"), root)
}

fn set_clock(agent: &Agent, now_ms: i64) {
    let path = agent.root().join("delegate-process-test-clock");
    let mut clock = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open the fixture clock");
    clock.set_len(0).expect("truncate the fixture clock");
    clock.rewind().expect("rewind the fixture clock");
    clock
        .write_all(now_ms.to_string().as_bytes())
        .expect("advance the fixture clock");
    clock.flush().expect("flush the fixture clock");
}

fn device(agent: &Agent) -> DeviceId {
    DeviceId::parse(&agent.delegate().hello().expect("delegate hello").device_id)
        .expect("the Agent names a device")
}

fn profile(client: &koru_api_client::Client, name: &str) -> ProfileId {
    client
        .store()
        .create_profile(name, None, None, Some("en"))
        .expect("create a profile")
        .id
}

fn explicit_product_wake(agent: &Agent, now_ms: i64) {
    set_clock(agent, now_ms);
    // The real content-free worker-control frame. The fixture authority is
    // accepted only in test-support builds; a shipping child still accepts
    // only the supervisor's per-launch secret.
    agent
        .delegate()
        .wake_schedules("delegate-process-test".into())
        .expect("wake the product scheduler");
}

fn schedule_header(
    owner: DeviceId,
    requested_by: ProfileId,
    workspace: koru_delegate_core::WorkspaceId,
    document: koru_delegate_core::DocumentId,
) -> ScheduleHeader {
    ScheduleHeader {
        schedule: koru_delegate_core::ScheduleId::mint(),
        created_at_ms: BASE_MINUTE,
        initial_owner: owner,
        definition: ScheduleDefinition {
            routine: None,
            revision: 1,
            effective_at_ms: BASE_MINUTE,
            template: DelegationTemplate {
                requested_by: ActorRef::Person {
                    profile: requested_by,
                },
                objective: Objective::new("Review the attached page").unwrap(),
                agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
                requested_model: None,
                scope: WorkspaceScope::Temporary,
                grants: std::collections::BTreeSet::from([workspace]),
                context: vec![ScheduleContextPointer::Document {
                    workspace,
                    document,
                }],
            },
            recurrence: RecurrenceRule::floating(CronExpression::parse("0 * * * *").unwrap()),
            lifecycle: ScheduleLifecycle::Active,
        },
    }
}

fn read_schedule(
    client: &koru_api_client::Client,
    workspace: koru_delegate_core::WorkspaceId,
    document: koru_delegate_core::DocumentId,
) -> ScheduleDocument {
    ScheduleDocument::from_snapshot(&client.store().snapshot(workspace, document).unwrap()).unwrap()
}

fn wait_for_completed_run(client: &koru_api_client::Client, run: RunId) {
    let (send, receive) = mpsc::sync_channel(8);
    let _subscription = client
        .runs()
        .stream(run, None, None, move |item| {
            let _ = send.try_send(item);
        })
        .expect("follow the scheduled run");
    loop {
        let item = receive
            .recv_timeout(PROCESS_DEADLINE)
            .expect("the fixture run to complete")
            .expect("a valid run stream item");
        if matches!(
            item,
            RunStreamItem::Status {
                status: RunStatus::Running { ref attention, .. }
            } if attention.contains(koru_delegate_core::Attention::FinishedNeedingAttention)
        ) {
            return;
        }
    }
}

fn provider_creates(agent: &Agent) -> usize {
    std::fs::read_to_string(agent.root().join("codex-fixture.trace"))
        .unwrap_or_default()
        .lines()
        .filter(|line| *line == "method:thread/start")
        .count()
}

fn pinned_document(
    client: &koru_api_client::Client,
    workspace: koru_delegate_core::WorkspaceId,
    run: RunId,
) -> (
    koru_delegate_core::DocumentVersionId,
    koru_delegate_api::wire::RunView,
) {
    let view = client
        .runs()
        .run(run, None, 0, 500)
        .expect("read the scheduled run");
    let stored = RunDocument::from_snapshot(
        &client
            .store()
            .snapshot(workspace, view.document)
            .expect("read the Run document"),
    )
    .expect("decode the Run document");
    assert_eq!(stored.header().unwrap(), view.header);
    let RunContextItem::Document { version, .. } = view.header.context[0] else {
        panic!("the scheduled pointer did not become a document pin")
    };
    (version, view)
}

fn materialized_page(
    agent: &Agent,
    run: RunId,
    document: koru_delegate_core::DocumentId,
    version: koru_delegate_core::DocumentVersionId,
) -> String {
    let workspace = agent
        .delegate_runs_base()
        .join(koru_settings::RUNS_FOLDER)
        .join(run.to_storage())
        .join("workspace");
    let context = koru_delegate_context::layout::run_dir(&workspace, run);
    std::fs::read_to_string(context.join(koru_delegate_context::layout::document_file(
        document, version,
    )))
    .expect("the scheduled pin was materialized")
}

fn trash_page(
    client: &koru_api_client::Client,
    workspace: koru_delegate_core::WorkspaceId,
    document: koru_delegate_core::DocumentId,
) {
    let manifest_id = client.store().manifest(workspace).unwrap();
    let snapshot = client.store().snapshot(workspace, manifest_id).unwrap();
    let mut manifest = Manifest::from_snapshot(&snapshot).unwrap();
    let before = manifest.version();
    manifest
        .trash_document(
            document,
            Timestamp::from_millis(BASE_MINUTE + 2 * OCCURRENCE_STEP),
            Some("test"),
        )
        .unwrap();
    let room = format!("workspace:{}:manifest", workspace.to_storage());
    let mut connection = Connection::open(&client.store().discovery()).unwrap();
    connection
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    connection.join(&room, &before).unwrap();
    let batch = connection
        .send_update(&room, manifest.export_updates_since(&before).unwrap())
        .unwrap();
    // `Ok(None)` is the 100 ms read timeout, not a closed connection: under a
    // loaded machine a durable ack can take longer than one read. A connection
    // that really ends is an `Err`, and the deadline bounds a store that never
    // answers.
    let deadline = std::time::Instant::now() + PROCESS_DEADLINE;
    loop {
        match connection.next_event().expect("manifest edit event") {
            Some(koru_api_client::Event::Acked { ack, .. }) if ack.batch == batch => {
                assert!(ack.durable, "{:?}", ack.status);
                return;
            }
            Some(_) => {}
            None => assert!(
                std::time::Instant::now() < deadline,
                "the manifest edit was never acknowledged"
            ),
        }
    }
}

#[test]
fn two_product_roots_fire_once_and_each_occurrence_pins_the_live_source() {
    let first = scheduled_agent(BASE_MINUTE - 60_000);
    let second = scheduled_agent(BASE_MINUTE - 60_000);
    let first_ref = &first;
    let second_ref = &second;
    first.diagnosed(std::panic::AssertUnwindSafe(|| {
        second.diagnosed(std::panic::AssertUnwindSafe(|| {
            scheduled_context_scenario(first_ref, second_ref)
        }))
    }));
}

fn scheduled_context_scenario(first: &Agent, second: &Agent) {
    let first_client = first.gateway();
    let second_client = second.gateway();
    let first_profile = profile(&first_client, "First owner");
    let second_profile = profile(&second_client, "Second replica");
    let workspace = first_client
        .store()
        .create_workspace(first_profile, "Scheduled notes")
        .unwrap()
        .id;
    let (page, _, line) = a_pinned_page(&first_client, workspace, CAPTURED_BODY);
    let header = schedule_header(device(&first), first_profile, workspace, page);
    let schedule = ScheduleDocument::create(&header).unwrap();
    let schedule_document = first_client
        .store()
        .import_document(
            workspace,
            "Schedule",
            SCHEDULE_KIND,
            schedule.export_snapshot().unwrap(),
        )
        .unwrap();

    let bundle =
        koru_store_wire::bundle::decode(&first_client.store().export_workspace(workspace).unwrap())
            .unwrap();
    assert_eq!(
        second_client
            .store()
            .import_workspace(&bundle, ImportMode::Restore, second_profile)
            .unwrap()
            .id,
        workspace
    );
    let second_schedule = ScheduleDocument::from_snapshot(
        &second_client
            .store()
            .snapshot(workspace, schedule_document)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(second_schedule.header().unwrap(), header);

    explicit_product_wake(first, BASE_MINUTE + 1);
    explicit_product_wake(second, BASE_MINUTE + 1);
    let after_first = read_schedule(&first_client, workspace, schedule_document);
    assert_eq!(
        after_first.firings().unwrap().len(),
        1,
        "schedule state: {:?}",
        after_first.state()
    );
    let first_firing = after_first.firings().unwrap().remove(0);
    assert_eq!(first_firing.outcome, ScheduleFiringOutcome::Fired);
    wait_for_completed_run(&first_client, first_firing.run);

    // Two restored roots share the same authority identity. Only its owner
    // claims the immutable occurrence.
    assert_eq!(provider_creates(&first), 1);
    assert_eq!(provider_creates(&second), 0);
    let first_slots = first.runs_db().slots_of(header.schedule).unwrap();
    assert_eq!(first_slots.len(), 1);
    assert_eq!(first_slots[0].run, first_firing.run);
    assert!(first.runs_db().intent(first_firing.run).unwrap().is_some());
    assert!(
        second
            .runs_db()
            .slots_of(header.schedule)
            .unwrap()
            .is_empty()
    );
    assert!(second.runs_db().intent(first_firing.run).unwrap().is_none());
    assert_eq!(first_client.runs().list(workspace).unwrap().runs.len(), 1);
    assert!(
        second_client
            .runs()
            .list(workspace)
            .unwrap()
            .runs
            .is_empty()
    );
    assert!(
        read_schedule(&second_client, workspace, schedule_document)
            .firings()
            .unwrap()
            .is_empty()
    );

    let (first_pin, first_view) = pinned_document(&first_client, workspace, first_firing.run);
    assert!(matches!(
        first_view.header.origin,
        RunOrigin::Scheduled {
            schedule,
            occurrence_ms: BASE_MINUTE,
            revision: 1,
        } if schedule == header.schedule
    ));
    let first_materialized = materialized_page(&first, first_firing.run, page, first_pin);
    assert_eq!(
        first_materialized,
        format!("# Attached\n\n{CAPTURED_BODY}\n")
    );

    edit_page(&first_client, workspace, page, line, EDITED_BODY);
    explicit_product_wake(first, BASE_MINUTE + OCCURRENCE_STEP + 1);
    explicit_product_wake(second, BASE_MINUTE + OCCURRENCE_STEP + 1);
    let after_second = read_schedule(&first_client, workspace, schedule_document);
    assert_eq!(after_second.firings().unwrap().len(), 2);
    let second_firing = after_second.firings().unwrap().remove(1);
    assert_eq!(second_firing.outcome, ScheduleFiringOutcome::Fired);
    wait_for_completed_run(&first_client, second_firing.run);
    let (second_pin, second_view) = pinned_document(&first_client, workspace, second_firing.run);
    assert_ne!(first_pin, second_pin);
    assert!(matches!(
        second_view.header.origin,
        RunOrigin::Scheduled {
            schedule,
            occurrence_ms,
            revision: 1,
        } if schedule == header.schedule && occurrence_ms == BASE_MINUTE + OCCURRENCE_STEP
    ));
    let second_materialized = materialized_page(&first, second_firing.run, page, second_pin);
    assert_eq!(
        second_materialized,
        format!("# Attached\n\n{EDITED_BODY}\n")
    );
    assert_eq!(
        materialized_page(&first, first_firing.run, page, first_pin),
        first_materialized,
        "the later source edit rewrote an earlier run's immutable input"
    );
    let first_version = first_client
        .store()
        .version_snapshot(workspace, first_pin)
        .unwrap()
        .expect("the first exact pin remains retained");
    let second_version = first_client
        .store()
        .version_snapshot(workspace, second_pin)
        .unwrap()
        .expect("the second exact pin remains retained");
    assert_ne!(first_version.snapshot, second_version.snapshot);
    assert_eq!(provider_creates(&first) + provider_creates(&second), 2);

    trash_page(&first_client, workspace, page);
    explicit_product_wake(first, BASE_MINUTE + 2 * OCCURRENCE_STEP + 1);
    explicit_product_wake(second, BASE_MINUTE + 2 * OCCURRENCE_STEP + 1);
    let paused = read_schedule(&first_client, workspace, schedule_document);
    assert!(matches!(
        paused.state().unwrap(),
        ScheduleState::Paused {
            reason: SchedulePauseReason::ContextUnavailable {
                item: 0,
                stage: ScheduleContextFailureStage::Resolve,
            },
            ..
        }
    ));
    assert_eq!(paused.firings().unwrap().len(), 2);
    assert_eq!(provider_creates(&first) + provider_creates(&second), 2);
    assert_eq!(first_client.runs().list(workspace).unwrap().runs.len(), 2);
    assert_eq!(
        first_client
            .store()
            .documents(workspace)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.kind == RUN_KIND)
            .count(),
        2
    );
}
