// SPDX-License-Identifier: MPL-2.0
use super::*;
fn gate() -> Gate {
    Gate {
        bootstrap: Bootstrap {
            bot: Uuid::new_v4(),
            incarnation: Uuid::new_v4(),
            root: PathBuf::new(),
            segment: "test".into(),
            capability: "a".repeat(64),
        },
        registration: AtomicU8::new(PENDING),
        stopping: AtomicBool::new(false),
        last_heard: AtomicU64::new(0),
        sessions: AtomicU32::new(0),
        deadline: Instant::now() + Duration::from_secs(10),
    }
}
fn request(gate: &Gate) -> Registration {
    Registration {
        bot: gate.bootstrap.bot,
        incarnation: gate.bootstrap.incarnation,
        capability: gate.bootstrap.capability.clone(),
    }
}
#[test]
fn registration_is_one_use_even_after_disconnect_and_rejects_stale_identity() {
    let gate = gate();
    let mut request = request(&gate);
    request.incarnation = Uuid::new_v4();
    assert!(!claim(&gate, &request, Instant::now()));
    request.incarnation = gate.bootstrap.incarnation;
    request.capability = "b".repeat(64);
    assert!(!claim(&gate, &request, Instant::now()));
    request.capability = gate.bootstrap.capability.clone();
    assert!(claim(&gate, &request, Instant::now()));
    assert!(!claim(&gate, &request, Instant::now()));
    gate.registration.store(DISCONNECTED, Ordering::Release);
    assert!(!claim(&gate, &request, Instant::now()));
    assert_eq!(gate.registration.load(Ordering::Acquire), DISCONNECTED);
}
#[test]
fn a_late_or_stopping_worker_cannot_register() {
    let gate = gate();
    let request = request(&gate);
    assert!(!claim(&gate, &request, gate.deadline));
    gate.stopping.store(true, Ordering::Release);
    assert!(!claim(&gate, &request, Instant::now()));
    assert!(!format!("{:?}", gate.bootstrap).contains(&gate.bootstrap.capability));
}
