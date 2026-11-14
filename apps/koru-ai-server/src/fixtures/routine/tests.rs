// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::{RoutineCall, RoutineCallContext};
use koru_delegate_api::bots::{BotCommand, BotReply};
use koru_delegate_api::routines::{
    PrepareOccurrenceReply, ReadPreparation, ReadPreparationReply, RoutineBridgeRefusal,
};
use koru_delegate_core::{BotHello, DelegateBootId, RoutineClaimObservation};
use std::{
    future::Future,
    pin::Pin,
    sync::mpsc::SyncSender,
    task::{Context, Poll, Wake, Waker},
    time::Instant,
};

struct Delegate {
    owner: koru_delegate_core::BotOwner,
    boot: DelegateBootId,
}
impl BotDelegatePort for Delegate {
    fn execute(&self, _: BotCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        panic!("only finite calls");
    }
    fn execute_within(
        &self,
        command: BotCommand,
        _: Duration,
    ) -> Result<BotReply, koru_delegate_api::Failure> {
        match command {
            BotCommand::Hello { owner, .. } => {
                assert_eq!(owner, self.owner);
                Ok(BotReply::Hello {
                    hello: BotHello {
                        boot: self.boot,
                        entries: vec![],
                        next: None,
                    },
                })
            }
            BotCommand::PendingNativeEffects { .. } => Err(koru_delegate_api::Failure::new(
                "unavailable",
                "fixture has no native effects",
                true,
            )),
            BotCommand::ReadRoutineClaim { slot } => {
                assert_eq!(slot.owner, self.owner);
                Ok(BotReply::RoutineClaim {
                    observation: RoutineClaimObservation::Absent {
                        slot,
                        observed_at_ms: slot.occurrence_ms + 1,
                    },
                })
            }
            other => panic!("unexpected delegate command: {other:?}"),
        }
    }
}
fn fixture() -> RoutineWorkerFixture {
    RoutineWorkerFixture::spawn(|request| {
        Arc::new(Delegate {
            owner: request.slot.owner,
            boot: DelegateBootId::mint(),
        })
    })
    .unwrap()
}
struct Signal(SyncSender<()>);
impl Wake for Signal {
    fn wake(self: Arc<Self>) {
        let _ = self.0.try_send(());
    }
}
fn wait<T>(mut call: RoutineCall<T>) -> Result<T, RoutineBridgeRefusal> {
    let (send, receive) = sync_channel(1);
    let waker = Waker::from(Arc::new(Signal(send)));
    let mut context = Context::from_waker(&waker);
    let end = Instant::now() + Duration::from_secs(8);
    loop {
        match Pin::new(&mut call).poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => receive
                .recv_timeout(end.saturating_duration_since(Instant::now()))
                .unwrap(),
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
fn ready(fixture: &RoutineWorkerFixture) -> ReadPreparation {
    let request = fixture.preparation();
    let read = ReadPreparation {
        slot: request.slot,
        semantic_hash: bot_routine_semantic_hash(
            &request.slot,
            &request.schedule_definition,
            request.workspace,
            None,
        )
        .unwrap(),
    };
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let reply = wait(
            fixture
                .client()
                .read_routine_preparation(context(), read.clone())
                .unwrap(),
        )
        .unwrap();
        if matches!(reply, ReadPreparationReply::Missing { .. }) {
            return read;
        }
        assert!(Instant::now() < end, "owner discovery pending: {reply:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn fixture_capture_hold_drop_releases_real_capture_and_returns_writer() {
    let fixture = fixture();
    let read = ready(&fixture);
    let hold = fixture.hold_next_capture();
    let call = fixture
        .client()
        .prepare_routine_occurrence(context(), fixture.preparation())
        .unwrap();
    assert!(hold.wait_entered(Duration::from_secs(4)));
    assert_eq!(fixture.capture_count(), 1);
    assert!(fixture.catalog_reads() >= 1);
    drop(hold);
    assert!(matches!(
        wait(call).unwrap(),
        PrepareOccurrenceReply::Prepared { .. }
    ));
    let repository = fixture.finish().unwrap();
    assert!(
        repository
            .read_routine_preparation(&read.slot, read.semantic_hash)
            .unwrap()
            .is_some()
    );
}

#[test]
fn fixture_finish_releases_held_real_capture_before_join() {
    let fixture = fixture();
    ready(&fixture);
    let hold = fixture.hold_next_capture();
    let call = fixture
        .client()
        .prepare_routine_occurrence(context(), fixture.preparation())
        .unwrap();
    assert!(hold.wait_entered(Duration::from_secs(4)));
    assert_eq!(fixture.capture_count(), 1);
    let started = Instant::now();
    let _repository = fixture.finish().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "finish must release before the gate's fallback timeout"
    );
    drop(call);
    drop(hold);
}
