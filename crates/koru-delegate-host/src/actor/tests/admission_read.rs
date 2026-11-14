// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_api::bots::{BotCommand, BotReply};
use koru_delegate_core::{BotAdmissionBarrier, BotBootAdmission, BotId, BotOwner, BotRequestId};

fn read_through_handle(actor: &mut Actor, owner: BotOwner, bot: BotId) -> BotReply {
    let (reply, response) = mpsc::sync_channel(1);
    actor.handle(Command::Bot {
        command: BotCommand::ReadAdmissionBarrier { owner, bot },
        reply,
    });
    let Response::Bot(reply) = response.try_recv().unwrap() else {
        panic!("typed admission reply required")
    };
    reply
}

#[test]
fn admission_read_actor_echoes_exact_absence_and_paused_without_wake_or_sessions() {
    let (mut actor, _, inbox) = actor();
    actor.device = DeviceId::new();
    let owner = BotOwner {
        user_scope: koru_store_core::UserScopeId::new(),
        device: actor.device,
    };
    let bot = BotId::mint();
    assert_eq!(
        read_through_handle(&mut actor, owner, bot),
        BotReply::AdmissionBarrier {
            owner,
            bot,
            admission: BotBootAdmission::Missing {}
        }
    );
    let barrier = BotAdmissionBarrier {
        owner,
        bot,
        request: BotRequestId::mint(),
        epoch: 1,
        paused: true,
    };
    actor
        .orchestrator
        .bot_barrier(&barrier, owner.device)
        .unwrap();
    for _ in 0..2 {
        assert_eq!(
            read_through_handle(&mut actor, owner, bot),
            BotReply::AdmissionBarrier {
                owner,
                bot,
                admission: BotBootAdmission::Present {
                    barrier: barrier.clone()
                }
            }
        );
    }
    let mut foreign = owner;
    foreign.device = DeviceId::new();
    assert!(
        actor
            .bot_command(BotCommand::ReadAdmissionBarrier {
                owner: foreign,
                bot
            })
            .is_err()
    );
    assert!(!actor.bot_drain_pending && !actor.bot_drain_again);
    assert!(matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Empty)));
    // The actual actor fixture panics on any document/session/capture call.
}
