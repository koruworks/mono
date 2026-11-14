// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn routine_foundation_actual_live_adapter_exposes_only_retained_typed_reader_facts() {
    let (h, doc, p, fresh) = fixture();
    let expected = claim(&h, &doc, &p, &fresh, false);
    let db = h.schedule_store.inner.lock().unwrap();
    let ledger: &dyn crate::RoutineLedger = &*db;
    let route = ledger.routine_route(p.slot.schedule).unwrap().unwrap();
    let held = ledger.held_routine_page(None).unwrap();
    assert_eq!(held.entries[0].1.as_ref().unwrap().record, expected);
    assert_eq!(
        ledger.queued_routine_page(None).unwrap().entries[0]
            .1
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .record,
        expected
    );
    let slots = ledger.routine_slots_page(&route, None).unwrap();
    assert_eq!(
        slots.entries[0].1.as_ref().unwrap().observation,
        RoutineClaimObservation::Claimed { record: expected }
    );
    assert!(crate::ScheduleLedger::unsettled(&*db).unwrap().is_empty());
    assert_eq!(db.unsettled_claims().unwrap().len(), 1);
    let candidate = crate::RoutineWork {
        evaluation_serial: 1,
        candidate: crate::RoutineCandidate::Recover {
            route,
            record: held.entries.into_iter().next().unwrap().1.unwrap().record,
        },
    };
    assert_eq!(format!("{candidate:?}"), "RoutineWork([REDACTED])");
}
