// SPDX-License-Identifier: MPL-2.0

//! Golden bytes of the Run view family (`RunView`, `RunExecution`,
//! `TimelineWindow`, `TimelineItem`).
//!
//! `GET /v1/runs/{run}`, the delegated API's
//! `read_run` all answer these bytes, and the Run viewer and the phone read
//! them. Written while the types lived in this crate's `wire` and kept
//! unchanged when they moved into `koru-delegate-core` (re-exported here
//! under the same path), so the move is proved not to have changed one byte.

use std::collections::BTreeSet;

use koru_delegate_core::{
    ActorRef, AmendmentRevision, Completeness, ContextState, DelegationRequest, DeviceId,
    DocumentId, ModelRef, Objective, ProfileId, RunAttemptId, RunHeader, RunId, RunStatus,
    SessionProfileKey, StepKind, TimelineEntry, TimelineEntryId, TimelineRefs, TurnOriginator,
    UsageFact, UsageFactId, WorkspaceId, WorkspaceScope,
};

use super::v4;
use crate::wire::{RunExecution, RunView, TimelineItem, TimelineWindow};

fn header() -> RunHeader {
    let mut header = RunHeader::from_request(
        DelegationRequest {
            run: RunId::from_uuid(v4(1)),
            requested_by: ActorRef::Person {
                profile: ProfileId::from_uuid(v4(3)),
            },
            objective: Objective::new("OBJECTIVE-SENTINEL").unwrap(),
            agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
            requested_model: None,
            scope: WorkspaceScope::Temporary,
            context: Vec::new(),
            grants: BTreeSet::from([WorkspaceId::from_uuid(v4(2))]),
            executing_device: DeviceId::from_uuid(v4(4)),
            origin: Default::default(),
        },
        1_000,
    );
    header.status = RunStatus::Completed;
    header
}

fn step() -> TimelineItem {
    TimelineItem::Step(TimelineEntry {
        id: TimelineEntryId::from_uuid(v4(5)),
        kind: StepKind::TurnStarted {
            originator: TurnOriginator::Orchestrator,
        },
        refs: TimelineRefs::default(),
        at_ms: 2_000,
    })
}

fn usage() -> TimelineItem {
    TimelineItem::Usage(UsageFact {
        identity: UsageFactId::from_uuid(v4(6)),
        attempt: RunAttemptId::from_uuid(v4(7)),
        turn: v4(8).hyphenated().to_string().parse().unwrap(),
        revision: AmendmentRevision(1),
        input: Some(12),
        output: Some(34),
        cached_input: None,
        model: ModelRef::from_uuid(v4(9)),
        completeness: Completeness::Complete,
    })
}

fn execution() -> RunExecution {
    RunExecution {
        device: DeviceId::from_uuid(v4(4)),
        session: Some(v4(10).hyphenated().to_string().parse().unwrap()),
        run_workspace: Some("C:/work/run".into()),
    }
}

fn window() -> TimelineWindow {
    TimelineWindow {
        total: 7,
        from: 4,
        items: vec![step(), usage(), TimelineItem::Unrecognized],
    }
}

fn view() -> RunView {
    RunView {
        document: DocumentId::from_uuid(v4(11)),
        header: header(),
        execution: execution(),
        context: vec![ContextState::Live],
        timeline: window(),
    }
}

/// The bytes are pinned, and the same bytes read back to the same value.
fn golden<T>(value: &T, expected: &str)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    assert_eq!(serde_json::to_string(value).unwrap(), expected);
    assert_eq!(&serde_json::from_str::<T>(expected).unwrap(), value);
}

const ITEM_STEP: &str = r#"{"entry":"step","id":"00000000-0000-4000-8000-000000000005","kind":{"step":"turn_started","originator":"orchestrator"},"refs":[],"at_ms":2000}"#;
const ITEM_USAGE: &str = r#"{"entry":"usage","identity":"00000000-0000-4000-8000-000000000006","attempt":"00000000-0000-4000-8000-000000000007","turn":"00000000-0000-4000-8000-000000000008","revision":1,"input":12,"output":34,"cached_input":null,"model":"00000000-0000-4000-8000-000000000009","completeness":{"kind":"complete"}}"#;
const ITEM_UNRECOGNIZED: &str = r#"{"entry":"unrecognized"}"#;
const EXECUTION: &str = r#"{"device":"00000000-0000-4000-8000-000000000004","session":"00000000-0000-4000-8000-00000000000a","run_workspace":"C:/work/run"}"#;
const EXECUTION_ELSEWHERE: &str = r#"{"device":"00000000-0000-4000-8000-000000000004"}"#;
const WINDOW: &str = r#"{"total":7,"from":4,"items":[{"entry":"step","id":"00000000-0000-4000-8000-000000000005","kind":{"step":"turn_started","originator":"orchestrator"},"refs":[],"at_ms":2000},{"entry":"usage","identity":"00000000-0000-4000-8000-000000000006","attempt":"00000000-0000-4000-8000-000000000007","turn":"00000000-0000-4000-8000-000000000008","revision":1,"input":12,"output":34,"cached_input":null,"model":"00000000-0000-4000-8000-000000000009","completeness":{"kind":"complete"}},{"entry":"unrecognized"}]}"#;
const VIEW: &str = r#"{"document":"00000000-0000-4000-8000-00000000000b","header":{"run":"00000000-0000-4000-8000-000000000001","created_at_ms":1000,"requested_by":{"kind":"person","profile":"00000000-0000-4000-8000-000000000003"},"objective":"OBJECTIVE-SENTINEL","agent":{"kind":"built_in","value":"codex"},"scope":{"kind":"temporary"},"context":[],"grants":["00000000-0000-4000-8000-000000000002"],"status":{"state":"completed"},"environment":{"kind":"local_device","device":"00000000-0000-4000-8000-000000000004"},"artifacts":[],"economics":{"orchestrator_turns":0,"human_turns":0,"turns_with_usage":0,"input":{"known":0,"unavailable_turns":0},"output":{"known":0,"unavailable_turns":0},"cached_input":{"known":0,"unavailable_turns":0},"tool_calls":0,"interventions":0,"elapsed_ms":null,"models":[],"complete":false},"origin":{"origin":"manual"}},"execution":{"device":"00000000-0000-4000-8000-000000000004","session":"00000000-0000-4000-8000-00000000000a","run_workspace":"C:/work/run"},"context":["live"],"timeline":{"total":7,"from":4,"items":[{"entry":"step","id":"00000000-0000-4000-8000-000000000005","kind":{"step":"turn_started","originator":"orchestrator"},"refs":[],"at_ms":2000},{"entry":"usage","identity":"00000000-0000-4000-8000-000000000006","attempt":"00000000-0000-4000-8000-000000000007","turn":"00000000-0000-4000-8000-000000000008","revision":1,"input":12,"output":34,"cached_input":null,"model":"00000000-0000-4000-8000-000000000009","completeness":{"kind":"complete"}},{"entry":"unrecognized"}]}}"#;

#[test]
fn a_timeline_item_has_its_golden_bytes() {
    golden(&step(), ITEM_STEP);
    golden(&usage(), ITEM_USAGE);
    golden(&TimelineItem::Unrecognized, ITEM_UNRECOGNIZED);
}

#[test]
fn a_run_execution_has_its_golden_bytes() {
    golden(&execution(), EXECUTION);
    // Another device, between attempts: nothing but the device.
    golden(
        &RunExecution {
            device: DeviceId::from_uuid(v4(4)),
            session: None,
            run_workspace: None,
        },
        EXECUTION_ELSEWHERE,
    );
}

#[test]
fn a_timeline_window_has_its_golden_bytes() {
    golden(&window(), WINDOW);
}

#[test]
fn a_run_view_has_its_golden_bytes() {
    golden(&view(), VIEW);
}
