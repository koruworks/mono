// SPDX-License-Identifier: MPL-2.0
//! Actual starter/worker/capture/Store composition, qualified on Windows catalog
//! IPC. The finite delegate is scripted; this does not qualify a live provider.
use super::*;
use koru_ai_server::{RoutineCallContext, fixtures::RoutineWorkerFixture};
use koru_delegate_api::{
    bots::{BotCommand, BotDelegatePort, BotReply},
    routines::*,
};
use koru_delegate_core::{
    AttemptGeneration, BotHello, DelegateBootId, RoutineClaimIdentity, RoutineClaimObservation,
    RoutineClaimRecord, RoutineLinkOutcome, RoutineLinkState, RunToolAuthority,
};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc::{Receiver, SyncSender, sync_channel},
};

struct Hold {
    entered: SyncSender<()>,
    release: Receiver<()>,
}
struct Release(Option<SyncSender<()>>);
impl Drop for Release {
    fn drop(&mut self) {
        if let Some(s) = self.0.take() {
            let _ = s.send(());
        }
    }
}
struct Peer {
    seed: PrepareOccurrence,
    boot: DelegateBootId,
    record: Mutex<Option<RoutineClaimRecord>>,
    hold: Mutex<Option<Hold>>,
    resolves: AtomicUsize,
    claim_unavailable: AtomicBool,
}
impl BotDelegatePort for Peer {
    fn execute(&self, _: BotCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        panic!("finite calls only")
    }
    fn execute_within(
        &self,
        command: BotCommand,
        within: Duration,
    ) -> Result<BotReply, koru_delegate_api::Failure> {
        assert!(!within.is_zero() && within <= Duration::from_secs(30));
        match command {
            BotCommand::Hello { owner, .. } => {
                assert_eq!(owner, self.seed.slot.owner);
                Ok(BotReply::Hello {
                    hello: BotHello {
                        boot: self.boot,
                        entries: vec![],
                        next: None,
                    },
                })
            }
            BotCommand::PendingNativeEffects { .. } => {
                Err(koru_delegate_api::Failure::unavailable())
            }
            BotCommand::ReadRoutineClaim { slot } => {
                assert_eq!(slot, self.seed.slot);
                if self.claim_unavailable.load(Ordering::Acquire) {
                    return Err(koru_delegate_api::Failure::unavailable());
                }
                Ok(BotReply::RoutineClaim {
                    observation: self.record.lock().unwrap().clone().map_or(
                        RoutineClaimObservation::Absent {
                            slot,
                            observed_at_ms: slot.occurrence_ms + 1,
                        },
                        |record| RoutineClaimObservation::Claimed { record },
                    ),
                })
            }
            BotCommand::AcknowledgeRoutineLink { acknowledgement } => {
                let mut r = self.record.lock().unwrap();
                let record = r.as_mut().unwrap();
                assert_eq!(record.claim, acknowledgement.claim);
                record.link_state = match acknowledgement.outcome {
                    RoutineLinkOutcome::Linked {} => RoutineLinkState::Linked {},
                    RoutineLinkOutcome::Terminal { reason } => {
                        RoutineLinkState::Terminal { reason }
                    }
                };
                Ok(BotReply::RoutineLinkAcknowledged {
                    record: record.clone(),
                })
            }
            BotCommand::ResolveRunAuthority { run, generation } => {
                self.resolves.fetch_add(1, Ordering::AcqRel);
                let hold = self.hold.lock().unwrap().take();
                if let Some(hold) = hold {
                    hold.entered.send(()).unwrap();
                    hold.release.recv_timeout(Duration::from_secs(8)).unwrap();
                }
                let record = self.record.lock().unwrap();
                let record = record.as_ref().unwrap();
                assert_eq!(run, record.claim.ids.run);
                assert_eq!(generation, AttemptGeneration::FIRST);
                Ok(BotReply::RunAuthority {
                    authority: RunToolAuthority::Bot {
                        provenance: record.submission.provenance.clone(),
                        policy: record.submission.policy.clone(),
                    },
                })
            }
            BotCommand::RegisterRoutineRoute { identity } => {
                assert_eq!(identity.owner, self.seed.slot.owner);
                assert_eq!(identity.bot, self.seed.slot.bot);
                Ok(BotReply::RoutineRoute { identity })
            }
            BotCommand::Reconcile { key } => {
                let r = self.record.lock().unwrap();
                let r = r.as_ref().unwrap();
                assert_eq!(key.request, r.claim.ids.submission);
                Ok(serde_json::from_value(json!({"result":"admission","admission":{"key":key,"run":r.claim.ids.run,"state":"queued","queue_sequence":1,"fence":null}})).unwrap())
            }
            other => panic!("unexpected provider/execution command: {other:?}"),
        }
    }
}
fn context() -> RoutineCallContext {
    RoutineCallContext::new(
        uuid::Uuid::new_v4(),
        Instant::now() + Duration::from_secs(5),
        Cancellation::default(),
    )
}
async fn setup() -> (RoutineWorkerFixture, Arc<Peer>, RunBotSubject) {
    let mut peer = None;
    let fixture = RoutineWorkerFixture::spawn(|seed| {
        let p = Arc::new(Peer {
            seed: seed.clone(),
            boot: DelegateBootId::mint(),
            record: Mutex::new(None),
            hold: Mutex::new(None),
            resolves: AtomicUsize::new(0),
            claim_unavailable: AtomicBool::new(false),
        });
        peer = Some(p.clone());
        p
    })
    .unwrap();
    let peer = peer.unwrap();
    let req = fixture.preparation();
    let read = ReadPreparation {
        slot: req.slot,
        semantic_hash: koru_ai_core::bots::bot_routine_semantic_hash(
            &req.slot,
            &req.schedule_definition,
            req.workspace,
            None,
        )
        .unwrap(),
    };
    let end = Instant::now() + Duration::from_secs(6);
    loop {
        let value = fixture
            .client()
            .read_routine_preparation(context(), read.clone())
            .unwrap()
            .await
            .unwrap();
        if matches!(value, ReadPreparationReply::Missing { .. }) {
            break;
        }
        assert!(Instant::now() < end, "actual owner discovery: {value:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let PrepareOccurrenceReply::Prepared { preparation } = fixture
        .client()
        .prepare_routine_occurrence(context(), req)
        .unwrap()
        .await
        .unwrap()
    else {
        panic!("actual capture");
    };
    let claim = RoutineClaimIdentity {
        slot: preparation.slot,
        definition: preparation.definition,
        ids: preparation.ids,
        preparation_hash: preparation.preparation_hash,
        claimed_at_ms: preparation.slot.occurrence_ms + 123,
    };
    *peer.record.lock().unwrap() = Some(RoutineClaimRecord {
        claim: claim.clone(),
        submission_version: 2,
        submission: preparation.submission.clone(),
        link_state: RoutineLinkState::Held {},
        updated_at_ms: claim.claimed_at_ms,
    });
    assert!(matches!(
        fixture
            .client()
            .finalize_routine_occurrence(context(), FinalizeOccurrence { claim })
            .unwrap()
            .await
            .unwrap(),
        FinalizeOccurrenceReply::Linked { .. }
    ));
    let subject = RunBotSubject {
        run: preparation.ids.run,
        generation: AttemptGeneration::FIRST,
        provenance: preparation.submission.provenance,
        captured: preparation.submission.policy,
    };
    (fixture, peer, subject)
}
#[tokio::test]
async fn routine_gateway_four_tools_invoke_real_worker_with_linked_routine_subject() {
    let (fixture, peer, subject) = setup().await;
    let before = fixture.capture_count();
    let listed = invoke(
        fixture.client(),
        subject.clone(),
        parse("list_routines", json!({})).unwrap(),
    )
    .await;
    assert_ne!(listed.is_error, Some(true));
    let revision = listed.structured_content.as_ref().unwrap()["bot_revision"]
        .as_u64()
        .unwrap();
    let id = BotRequestId::mint();
    let mut args = create();
    args["request_id"] = json!(id);
    args["expected_bot_revision"] = json!(revision);
    let created = invoke(
        fixture.client(),
        subject.clone(),
        parse("create_routine", args.clone()).unwrap(),
    )
    .await;
    assert_ne!(created.is_error, Some(true), "{created:?}");
    let schedule = created.structured_content.as_ref().unwrap()["schedule_id"].clone();
    let read = invoke(
        fixture.client(),
        subject.clone(),
        parse("read_routine_creation", json!({"request_id":id})).unwrap(),
    )
    .await;
    assert_ne!(read.is_error, Some(true));
    assert_eq!(
        read.structured_content.as_ref().unwrap()["schedule_id"],
        schedule
    );
    let retry=invoke(fixture.client(),subject.clone(),parse("retry_routine_creation",json!({"original_request_id":id,"observed_token":read.structured_content.as_ref().unwrap()["observed_token"]})).unwrap()).await;
    if retry.is_error == Some(true) {
        assert_eq!(
            retry.structured_content.as_ref().unwrap()["code"],
            "request_conflict"
        );
    } else {
        assert_eq!(
            retry.structured_content.as_ref().unwrap()["schedule_id"],
            schedule
        );
    }
    let replay = invoke(
        fixture.client(),
        subject.clone(),
        parse("create_routine", args).unwrap(),
    )
    .await;
    assert_ne!(replay.is_error, Some(true));
    assert_eq!(
        replay.structured_content.as_ref().unwrap()["schedule_id"],
        schedule
    );
    assert_eq!(fixture.capture_count(), before);
    assert!(peer.resolves.load(Ordering::Acquire) >= 20);
    let repo = fixture.finish().unwrap();
    assert!(
        repo.routine_tool_creation_by_request(subject.provenance.owner, subject.provenance.bot, id)
            .unwrap()
            .is_some()
    );
}
#[tokio::test]
async fn routine_gateway_dropped_future_cancels_real_worker_before_journal_acceptance() {
    let (fixture, peer, subject) = setup().await;
    let (send, entered) = sync_channel(1);
    let (release, wait) = sync_channel(1);
    let release = Release(Some(release));
    *peer.hold.lock().unwrap() = Some(Hold {
        entered: send,
        release: wait,
    });
    let id = BotRequestId::mint();
    let mut args = create();
    args["request_id"] = json!(id);
    let future = tokio::spawn(invoke(
        fixture.client(),
        subject.clone(),
        parse("create_routine", args).unwrap(),
    ));
    tokio::task::spawn_blocking(move || entered.recv_timeout(Duration::from_secs(5)).unwrap())
        .await
        .unwrap();
    future.abort();
    assert!(future.await.unwrap_err().is_cancelled());
    drop(release);
    let listed = invoke(
        fixture.client(),
        subject.clone(),
        parse("list_routines", json!({})).unwrap(),
    )
    .await;
    assert_ne!(listed.is_error, Some(true));
    let repo = fixture.finish().unwrap();
    assert!(
        repo.routine_tool_creation_by_request(subject.provenance.owner, subject.provenance.bot, id)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn routine_gateway_fresh_routine_create_requires_independent_current_claim() {
    let (fixture, peer, subject) = setup().await;
    peer.claim_unavailable.store(true, Ordering::Release);
    let id = BotRequestId::mint();
    let mut args = create();
    args["request_id"] = json!(id);
    let reply = invoke(
        fixture.client(),
        subject.clone(),
        parse("create_routine", args).unwrap(),
    )
    .await;
    assert_eq!(reply.is_error, Some(true));
    // Metadata observation still uses the full local Linked decoder plus current
    // Run/human authority, without mistaking it for a fresh independent claim.
    let listed = invoke(
        fixture.client(),
        subject.clone(),
        parse("list_routines", json!({})).unwrap(),
    )
    .await;
    assert_ne!(listed.is_error, Some(true));
    let repo = fixture.finish().unwrap();
    assert!(
        repo.routine_tool_creation_by_request(subject.provenance.owner, subject.provenance.bot, id)
            .unwrap()
            .is_none()
    );
}
