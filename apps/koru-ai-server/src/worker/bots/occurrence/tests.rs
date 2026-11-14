// SPDX-License-Identifier: MPL-2.0
use super::*;

mod identity;
// The existing IPC Endpoint uses filesystem sockets on Unix. These actual
// catalog/worker composition cases use Windows named pipes to keep all tests
// free of filesystem and real provider/session effects.
#[cfg(windows)]
mod worker;

fn slot() -> RoutineSlot {
    RoutineSlot {
        owner: koru_delegate_core::BotOwner {
            user_scope: koru_ai_core::bots::UserScopeId::new(),
            device: koru_delegate_core::DeviceId::new(),
        },
        bot: BotId::mint(),
        schedule: koru_delegate_core::ScheduleId::mint(),
        occurrence_ms: 60_000,
    }
}
fn operation(driver: &mut OccurrenceDriver, slot: RoutineSlot, serial: u64) -> Operation {
    let context = RoutineCallContext::new(
        Uuid::new_v4(),
        Instant::now() + ROUTINE_BRIDGE_DEADLINE,
        Cancellation::default(),
    );
    let hash = RoutineDigest32::from_bytes([1; 32]);
    let key = Key {
        request_id: context.request_id,
        serial,
        owner_epoch: driver.epoch,
        slot,
        identity: Identity::Semantic(hash),
        phase: 1,
    };
    driver.active.insert(
        serial,
        Active {
            key: key.clone(),
            cancellation: context.cancellation.clone(),
        },
    );
    Operation {
        key,
        context,
        request: Request::Recover(ReadPreparation {
            slot,
            semantic_hash: hash,
        }),
        reply: Reply::Recovery,
        _permit: driver.gates.acquire(false).unwrap(),
    }
}
