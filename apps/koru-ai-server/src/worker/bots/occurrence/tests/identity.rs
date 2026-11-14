// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn occurrence_old_epoch_and_stale_phase_cannot_retire_current_recovery_or_release_its_permit() {
    let slot = slot();
    let gates = RoutineCallGates::default();
    let mut driver = OccurrenceDriver::new(gates.clone());
    driver.synchronize(Some(ai_owner(slot)), 1);
    let old = operation(&mut driver, slot, 1);
    driver.synchronize(Some(ai_owner(slot)), 2);
    assert!(old.context.cancellation.is_cancelled());
    let mut current = operation(&mut driver, slot, 2);
    driver.recovery.started();
    assert!(gates.acquire(false).is_err());
    driver.finish(&old, true);
    assert!(driver.recovery.running());
    assert!(
        gates.acquire(false).is_err(),
        "return identity alone never releases the physical permit"
    );
    drop(old);
    let spare = gates.acquire(false).unwrap();
    driver.active.get_mut(&2).unwrap().key.phase = 2;
    driver.finish(&current, true);
    assert!(driver.recovery.running());
    assert!(driver.active.contains_key(&2));
    current.key.phase = 2;
    driver.finish(&current, false);
    assert!(!driver.recovery.running());
    assert!(!driver.active.contains_key(&2));
    assert!(gates.acquire(false).is_err());
    drop(current);
    assert!(gates.acquire(false).is_ok());
    drop(spare);
}

#[test]
fn occurrence_current_key_requires_uuid_serial_owner_epoch_slot_hash_and_phase() {
    let slot = slot();
    let mut driver = OccurrenceDriver::new(Default::default());
    driver.synchronize(Some(ai_owner(slot)), 9);
    let op = operation(&mut driver, slot, 4);
    assert!(driver.current(&op.key));
    let mut changes = vec![op.key.clone(); 6];
    changes[0].request_id = Uuid::new_v4();
    changes[1].serial += 1;
    changes[2].owner_epoch += 1;
    changes[3].slot.occurrence_ms += 60_000;
    changes[4].identity = Identity::Semantic(RoutineDigest32::from_bytes([2; 32]));
    changes[5].phase += 1;
    for key in changes {
        assert!(!driver.current(&key));
    }
    driver.cancel_all();
    assert!(op.context.cancellation.is_cancelled());
}

#[test]
fn preclaim_stale_identity_or_epoch_completion_cannot_finish_current_operation() {
    let slot = slot();
    let mut driver = OccurrenceDriver::new(Default::default());
    driver.synchronize(Some(ai_owner(slot)), 9);
    let mut op = operation(&mut driver, slot, 4);
    let request = SynchronizeRoutinePreclaim {
        slot,
        semantic_hash: RoutineDigest32::from_bytes([1; 32]),
        preparation_hash: RoutineDigest32::from_bytes([2; 32]),
    };
    op.request = Request::Preclaim(request.clone());
    op.key.identity = op.request.identity().unwrap();
    driver.active.get_mut(&4).unwrap().key = op.key.clone();
    for field in 0..7 {
        let mut key = op.key.clone();
        match field {
            0 => key.request_id = Uuid::new_v4(),
            1 => key.serial += 1,
            2 => key.owner_epoch += 1,
            3 => key.slot.occurrence_ms += 60_000,
            4 => {
                key.identity = Identity::Preparation {
                    semantic_hash: RoutineDigest32::from_bytes([3; 32]),
                    preparation_hash: request.preparation_hash,
                }
            }
            5 => {
                key.identity = Identity::Preparation {
                    semantic_hash: request.semantic_hash,
                    preparation_hash: RoutineDigest32::from_bytes([3; 32]),
                }
            }
            _ => key.phase += 1,
        }
        assert!(!driver.current(&key));
    }
    driver.active.get_mut(&4).unwrap().key.phase += 1;
    driver.finish(&op, false);
    assert!(driver.active.contains_key(&4));
}
