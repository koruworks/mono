// SPDX-License-Identifier: MPL-2.0

//! The wire round-trips, the scopes parse, and the frames carry what the
//! worker needs without a path or an objective in any failure.

use std::collections::BTreeSet;

use koru_delegate_core::{
    ActorRef, DeviceId, DocumentId, Objective, ProfileId, RunId, RunStatus, ScheduleId,
    SessionProfileKey, WorkspaceId, WorkspaceScope,
};
use koru_delegate_schedule::{
    DelegationTemplate, RecurrenceRule, ScheduleDefinition, ScheduleFiring, ScheduleFiringOutcome,
    ScheduleHeader, ScheduleLifecycle, ScheduleName, ScheduleState,
};
use uuid::Uuid;

use crate::ipc::{Caller, Request, Response, WIRE_VERSION};
use crate::*;

fn v4(n: u8) -> Uuid {
    format!("00000000-0000-4000-8000-0000000000{n:02x}")
        .parse()
        .unwrap()
}

fn request() -> CreateRunRequest {
    CreateRunRequest {
        run: RunId::from_uuid(v4(1)),
        workspace: WorkspaceId::from_uuid(v4(2)),
        requested_by: ActorRef::Person {
            profile: ProfileId::from_uuid(v4(3)),
        },
        objective: Objective::new("OBJECTIVE-SENTINEL").unwrap(),
        agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
        requested_model: None,
        scope: WorkspaceScope::Temporary,
        context: Vec::new(),
        grants: BTreeSet::from([WorkspaceId::from_uuid(v4(2))]),
    }
}

fn schedule_request() -> CreateScheduleRequest {
    CreateScheduleRequest {
        schedule: ScheduleId::from_uuid(v4(5)),
        workspace: WorkspaceId::from_uuid(v4(2)),
        name: koru_delegate_schedule::ScheduleName::new("SCHEDULE-NAME-SENTINEL").unwrap(),
        template: DelegationTemplate {
            requested_by: ActorRef::Person {
                profile: ProfileId::from_uuid(v4(3)),
            },
            objective: Objective::new("SCHEDULE-OBJECTIVE-SENTINEL").unwrap(),
            agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
            requested_model: Some(koru_delegate_core::ModelName::new("gpt-test").unwrap()),
            scope: WorkspaceScope::Temporary,
            grants: BTreeSet::from([WorkspaceId::from_uuid(v4(2))]),
            context: Vec::new(),
        },
        recurrence: RecurrenceRule::floating("0 9 * * MON-FRI".parse().unwrap()),
    }
}

#[test]
fn the_request_and_the_frames_round_trip() {
    let request = request();
    let json = serde_json::to_string(&request).unwrap();
    let back: CreateRunRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(back, request);
    // A request that names no context and no grants still reads.
    let minimal: CreateRunRequest = serde_json::from_value(serde_json::json!({
        "run": RunId::from_uuid(v4(1)).to_storage(),
        "workspace": WorkspaceId::from_uuid(v4(2)).to_storage(),
        "requested_by": {"kind": "person", "profile": ProfileId::from_uuid(v4(3)).to_storage()},
        "objective": "do the thing",
        "agent": {"kind": "built_in", "value": "codex"},
        "scope": {"kind": "temporary"},
    }))
    .unwrap();
    assert!(minimal.context.is_empty());
    assert!(minimal.grants.is_empty());

    let frame = Request::Create {
        caller: Caller {
            product: "koru-api-gateway:window".into(),
            trace_id: "t".into(),
        },
        request: request.clone(),
    };
    let bytes = koru_ipc::frame::encode(WIRE_VERSION, &frame).unwrap();
    let (version, decoded): (u8, Request) =
        koru_ipc::frame::read_frame(&mut bytes.as_slice()).unwrap();
    assert_eq!(version, WIRE_VERSION);
    assert_eq!(decoded, frame);
    let response = Response::Created(CreateRunResponse {
        run: request.run,
        document: koru_delegate_core::DocumentId::from_uuid(v4(9)),
        status: RunStatus::Created,
    });
    let bytes = koru_ipc::frame::encode(WIRE_VERSION, &response).unwrap();
    let (_, decoded): (u8, Response) = koru_ipc::frame::read_frame(&mut bytes.as_slice()).unwrap();
    assert_eq!(decoded, response);
}

#[test]
fn the_scopes_and_the_capability_are_spelled_once() {
    assert_eq!(CAPABILITY, "delegate");
    assert_eq!(DelegateScope::parse("runs.read"), Some(DelegateScope::Read));
    assert_eq!(
        DelegateScope::parse("runs.write"),
        Some(DelegateScope::Write)
    );
    assert_eq!(DelegateScope::parse("artifacts.read"), None);
    assert_eq!(DelegateScope::Read.as_str(), "runs.read");
}

#[test]
fn a_window_is_bounded_and_defaults() {
    let query: TimelineQuery = serde_json::from_str("{}").unwrap();
    assert_eq!(query.from, 0);
    assert_eq!(query.bounded_len(), 100);
    // A reader that does not name a workspace is asking about a run this
    // device created; naming one is how a second device finds a run that
    // reached it by sync.
    assert_eq!(query.workspace, None);
    let wide = TimelineQuery {
        from: 5,
        len: 10_000,
        workspace: None,
    };
    assert_eq!(wide.bounded_len(), wire::MAX_WINDOW as usize);
    assert_eq!(
        TimelineQuery {
            from: 0,
            len: 0,
            workspace: None
        }
        .bounded_len(),
        1
    );
}

#[test]
fn a_failure_and_a_request_debug_carry_no_objective() {
    let failure = Failure::unavailable();
    assert_eq!(failure.code, "delegate_unavailable");
    assert!(failure.retryable);
    let rendered = format!("{:?}", request());
    assert!(!rendered.contains("OBJECTIVE-SENTINEL"), "{rendered}");
    let frame = format!(
        "{:?}",
        Request::Create {
            caller: Caller {
                product: String::new(),
                trace_id: String::new()
            },
            request: request()
        }
    );
    assert!(!frame.contains("OBJECTIVE-SENTINEL"), "{frame}");
    let stream = RunStreamItem::Status {
        status: RunStatus::Completed,
    };
    let json = serde_json::to_value(&stream).unwrap();
    assert_eq!(json["kind"], "status");
    let _ = DeviceId::from_uuid(v4(4));
}

#[test]
fn a_local_delegate_with_no_worker_is_unavailable_not_a_panic() {
    let endpoint = koru_ipc::Endpoint::for_service_channel_root(
        koru_ipc::DELEGATE,
        "stable",
        std::path::Path::new("/nowhere/koru-delegate-api-test"),
    )
    .unwrap();
    let local = LocalDelegate::new(endpoint, "test");
    assert!(!local.serving());
    let context = RequestContext {
        principal: DelegatePrincipal {
            label: "test".into(),
            scopes: vec![DelegateScope::Read],
        },
        trace_id: "t".into(),
    };
    let error = local
        .list(context, WorkspaceId::from_uuid(v4(2)))
        .unwrap_err();
    assert_eq!(error.code, "delegate_unavailable");
}

/// A stream item is itself tagged `kind`; the frame's own tag must not
/// collide with it, or an item frame writes the key twice and refuses to
/// read itself back.
#[test]
fn an_item_frame_round_trips_beside_the_items_own_tag() {
    let item = Response::Item(RunStreamItem::Status {
        status: RunStatus::Completed,
    });
    let bytes = koru_ipc::frame::encode(WIRE_VERSION, &item).unwrap();
    let (_, decoded): (u8, Response) = koru_ipc::frame::read_frame(&mut bytes.as_slice()).unwrap();
    assert_eq!(decoded, item);
}

#[test]
fn explicit_model_intent_round_trips_without_entering_debug() {
    let mut chosen = request();
    chosen.requested_model =
        Some(koru_delegate_core::ModelName::new("private-provider/MODEL-SENTINEL").unwrap());
    let json = serde_json::to_value(&chosen).unwrap();
    assert_eq!(json["requested_model"], "private-provider/MODEL-SENTINEL");
    assert_eq!(
        serde_json::from_value::<CreateRunRequest>(json).unwrap(),
        chosen
    );
    assert!(!format!("{chosen:?}").contains("MODEL-SENTINEL"));
    assert_eq!(request().requested_model, None);
}

#[test]
fn schedule_create_carries_intent_but_no_authority_fields() {
    let request = schedule_request();
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(value["schedule"], request.schedule.to_storage());
    assert_eq!(value["template"]["requested_model"], "gpt-test");
    for worker_owned in [
        "owner",
        "initial_owner",
        "revision",
        "created_at_ms",
        "effective_at_ms",
        "lifecycle",
    ] {
        assert!(value.get(worker_owned).is_none(), "{worker_owned}");
    }
    assert!(!format!("{request:?}").contains("SCHEDULE-OBJECTIVE-SENTINEL"));
    assert!(!format!("{request:?}").contains("SCHEDULE-NAME-SENTINEL"));

    let frame = Request::CreateSchedule {
        caller: Caller {
            product: "koru-api-gateway:window".into(),
            trace_id: "t".into(),
        },
        request: request.clone(),
    };
    let bytes = koru_ipc::frame::encode(WIRE_VERSION, &frame).unwrap();
    let (version, decoded): (u8, Request) =
        koru_ipc::frame::read_frame(&mut bytes.as_slice()).unwrap();
    assert_eq!(version, WIRE_VERSION);
    assert_eq!(decoded, frame);
}

#[test]
fn skipped_claim_ids_never_cross_as_runs() {
    let skipped = RunId::from_uuid(v4(6));
    let holder = RunId::from_uuid(v4(7));
    let view = ScheduleFiringView::from(ScheduleFiring {
        occurrence_ms: 60_000,
        revision: 1,
        run: skipped,
        outcome: ScheduleFiringOutcome::SkippedBusy,
        busy_holder: Some(holder),
        at: 60_001,
    });
    assert_eq!(view.run, None);
    assert_eq!(view.busy_holder, Some(holder));

    let fired = ScheduleFiringView::from(ScheduleFiring {
        occurrence_ms: 120_000,
        revision: 1,
        run: skipped,
        outcome: ScheduleFiringOutcome::Fired,
        busy_holder: None,
        at: 120_001,
    });
    assert_eq!(fired.run, Some(skipped));
}

#[test]
fn schedule_change_is_content_free_and_restart_safe() {
    let change = ScheduleChange {
        instance: v4(8),
        generation: 4,
    };
    let value = serde_json::to_value(&change).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 2);
    assert_eq!(
        serde_json::from_value::<ScheduleChange>(value).unwrap(),
        change
    );

    let frame = Response::ScheduleChanged(change);
    let bytes = koru_ipc::frame::encode(WIRE_VERSION, &frame).unwrap();
    let (version, decoded): (u8, Response) =
        koru_ipc::frame::read_frame(&mut bytes.as_slice()).unwrap();
    assert_eq!(version, WIRE_VERSION);
    assert_eq!(decoded, frame);
}

#[test]
fn a_stale_v5_hello_is_incompatible_before_its_old_shape_is_decoded() {
    let legacy = serde_json::json!({
        "frame": "hello",
        "launch_id": "old-worker",
        "pid": 42,
        "device_id": "old-device",
        "active_runs": 0,
        "counters": {
            "intents_recorded": 1,
            "attempts_opened": 0,
            "contexts_captured": 0,
            "captures_failed": 0,
            "publications_queued": 0,
            "artifacts_registered": 0,
            "publications_acknowledged": 0,
            "publication_retries": 0,
            "restarts": 0,
            "restart_completed_forward": 0,
            "restart_started": 0,
            "restart_reattached": 0,
            "restart_reconciled": 0,
            "restart_interrupted": 0,
            "restart_unresolved": 0
        }
    });
    let bytes = koru_ipc::frame::encode(5, &legacy).unwrap();
    let failure = crate::local::read_current_response(&mut bytes.as_slice()).unwrap_err();
    assert_eq!(failure.code, "delegate_incompatible");
    assert!(!failure.retryable);
}

fn schedule_view() -> ScheduleView {
    let request = schedule_request();
    ScheduleView {
        workspace: request.workspace,
        document: DocumentId::from_uuid(v4(9)),
        name: Some(request.name),
        header: ScheduleHeader {
            schedule: request.schedule,
            created_at_ms: 10,
            initial_owner: DeviceId::from_uuid(v4(8)),
            definition: ScheduleDefinition {
                revision: 1,
                effective_at_ms: 10,
                template: request.template,
                recurrence: request.recurrence,
                lifecycle: ScheduleLifecycle::Active,
            },
        },
        state: ScheduleState::Active {
            owner: DeviceId::from_uuid(v4(8)),
        },
        next_firing: None,
        firings: Vec::new(),
    }
}

#[test]
fn v6_carries_every_schedule_management_request_and_response() {
    let view = schedule_view();
    let caller = Caller {
        product: "test".into(),
        trace_id: "trace".into(),
    };
    let expected_name = view.name.clone().unwrap();
    let replacement = ScheduleName::new("Replacement").unwrap();
    let requests = vec![
        Request::RenameSchedule {
            caller: caller.clone(),
            request: RenameScheduleRequest {
                schedule: view.header.schedule,
                workspace: view.workspace,
                expected_name: expected_name.clone(),
                name: replacement.clone(),
            },
        },
        Request::EditSchedule {
            caller: caller.clone(),
            request: EditScheduleRequest {
                schedule: view.header.schedule,
                workspace: view.workspace,
                expected_revision: 1,
                expected_name,
                name: replacement,
                template: view.header.definition.template.clone(),
                recurrence: view.header.definition.recurrence.clone(),
            },
        },
        Request::PauseSchedule {
            caller: caller.clone(),
            request: PauseScheduleRequest {
                schedule: view.header.schedule,
                workspace: view.workspace,
                expected_revision: 1,
            },
        },
        Request::ResumeSchedule {
            caller: caller.clone(),
            request: ResumeScheduleRequest {
                schedule: view.header.schedule,
                workspace: view.workspace,
                expected_revision: 1,
            },
        },
        Request::RetireSchedule {
            caller,
            request: RetireScheduleRequest {
                schedule: view.header.schedule,
                workspace: view.workspace,
            },
        },
    ];
    for request in requests {
        let bytes = koru_ipc::frame::encode(WIRE_VERSION, &request).unwrap();
        let (version, decoded): (u8, Request) =
            koru_ipc::frame::read_frame(&mut bytes.as_slice()).unwrap();
        assert_eq!(version, WIRE_VERSION);
        assert_eq!(decoded, request);
    }

    let responses = vec![
        Response::ScheduleRenamed(RenameScheduleResponse {
            schedule: view.clone(),
        }),
        Response::ScheduleEdited(EditScheduleResponse {
            schedule: view.clone(),
        }),
        Response::SchedulePaused(PauseScheduleResponse {
            schedule: view.clone(),
        }),
        Response::ScheduleResumed(ResumeScheduleResponse {
            schedule: view.clone(),
        }),
        Response::ScheduleRetired(RetireScheduleResponse { schedule: view }),
    ];
    for response in responses {
        let bytes = koru_ipc::frame::encode(WIRE_VERSION, &response).unwrap();
        let (version, decoded): (u8, Response) =
            koru_ipc::frame::read_frame(&mut bytes.as_slice()).unwrap();
        assert_eq!(version, WIRE_VERSION);
        assert_eq!(decoded, response);
    }
}
