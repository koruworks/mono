// SPDX-License-Identifier: MPL-2.0

//! Pure unit tests over in-memory documents. Nothing here touches disk, a
//! store, a process or a network; the two-root and through-the-store
//! properties live in `koru-store-sqlite/tests/run_document.rs`, which is
//! where a store exists.

use std::collections::BTreeSet;

use koru_delegate_core::*;
use uuid::Uuid;

use crate::*;
// Both globs name a `TimelineItem` since the Run view moved into the run
// vocabulary (`koru_delegate_core::TimelineItem`, the wire's); the window
// this crate reads is its own codec's.
use crate::TimelineItem;

mod bots;
mod dependency;
mod economics;
mod gate;
mod projection;
mod schedule;
mod shape;
mod subject;
mod timeline;

/// A deterministic v4 UUID, so a fixture is the same on every run.
pub(crate) fn v4(n: u8) -> Uuid {
    format!("00000000-0000-4000-8000-0000000000{n:02x}")
        .parse()
        .unwrap()
}

pub(crate) fn device(n: u8) -> DeviceId {
    DeviceId::from_uuid(v4(n))
}

pub(crate) fn attempt(n: u8) -> RunAttemptId {
    RunAttemptId::from_uuid(v4(n))
}

pub(crate) fn turn(n: u8) -> AgentTurnId {
    v4(n).hyphenated().to_string().parse().unwrap()
}

pub(crate) fn model(n: u8) -> ModelRef {
    ModelRef::from_uuid(v4(n))
}

pub(crate) const OBJECTIVE_SENTINEL: &str = "OBJECTIVE-SENTINEL tidy the release notes";
pub(crate) const NOTE_SENTINEL: &str = "NOTE-SENTINEL good enough";
pub(crate) const TITLE_SENTINEL: &str = "TITLE-SENTINEL notes.md";

pub(crate) fn request() -> DelegationRequest {
    DelegationRequest {
        run: RunId::from_uuid(v4(1)),
        requested_by: ActorRef::Person {
            profile: ProfileId::from_uuid(v4(2)),
        },
        objective: Objective::new(OBJECTIVE_SENTINEL).unwrap(),
        agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
        requested_model: None,
        scope: WorkspaceScope::ProjectRoot {
            project: v4(3).hyphenated().to_string().parse().unwrap(),
        },
        context: vec![RunContextItem::Document {
            workspace: WorkspaceId::from_uuid(v4(4)),
            document: DocumentId::from_uuid(v4(5)),
            version: DocumentVersionId::from_uuid(v4(6)),
        }],
        grants: BTreeSet::from([WorkspaceId::from_uuid(v4(4))]),
        executing_device: device(9),
        origin: Default::default(),
    }
}

pub(crate) fn header() -> RunHeader {
    RunHeader::from_request(request(), 1_000)
}

pub(crate) fn result(n: u8) -> ResultReference {
    ResultReference {
        artifact: ArtifactVersionId::from_uuid(v4(n)),
        held_by: device(9),
        title: ArtifactTitle::new(TITLE_SENTINEL).unwrap(),
        size_bytes: 4_096,
        registered_at_ms: 5_000,
    }
}

pub(crate) fn rating() -> RunRating {
    RunRating {
        rating: Rating::Good,
        note: Some(RatingNote::new(NOTE_SENTINEL).unwrap()),
        rated_by: ActorRef::Person {
            profile: ProfileId::from_uuid(v4(2)),
        },
        rated_at_ms: 9_000,
    }
}

pub(crate) fn step(kind: StepKind, at_ms: i64) -> TimelineEntry {
    TimelineEntry::new(kind, TimelineRefs::default(), at_ms)
}

/// A usage fact for `(attempt 1, turn n)` at `revision` reporting `input`.
pub(crate) fn usage(turn_n: u8, revision: u32, input: u64) -> UsageFact {
    UsageFact {
        identity: UsageFactId::from_uuid(v4(0x40 + turn_n)),
        attempt: attempt(0x20),
        turn: turn(turn_n),
        revision: AmendmentRevision(revision),
        input: Some(input),
        output: Some(10),
        cached_input: None,
        model: model(0x30),
        completeness: Completeness::Complete,
    }
}

pub(crate) fn identity(n: u8) -> ModelIdentity {
    ModelIdentity {
        id: model(n),
        provider: "codex".parse().unwrap(),
        requested: ModelReport::Named {
            name: ModelName::new("gpt-5-codex").unwrap(),
        },
        reported: ModelReport::Unknown,
    }
}

/// A run with `steps` turn-shaped steps appended one commit at a time.
pub(crate) fn a_run_with(steps: usize) -> RunDocument {
    let run = RunDocument::create(&header()).unwrap();
    for i in 0..steps {
        run.append_step(&step(
            StepKind::ToolsCompleted {
                calls: (i % 5) as u32,
                failed: 0,
                failures: Default::default(),
            },
            1_000 + i as i64,
        ))
        .unwrap();
    }
    run
}
