// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::messages::{Delivery, Envelope, Person, Publish, ReplyTo, Resume};

struct Fixture {
    db: Database,
    admission: Admission,
    worker: Uuid,
}
impl Fixture {
    fn new(limit: u32) -> Self {
        Self {
            db: Database::in_memory().unwrap(),
            admission: Admission::new(limit),
            worker: Uuid::new_v4(),
        }
    }
    fn key(&self, bot: u128, thread: u128) -> SessionKey {
        SessionKey {
            bot: Uuid::from_u128(bot),
            thread: Uuid::from_u128(thread),
        }
    }
    fn send(&mut self, key: SessionKey, at: i64) {
        self.db.connection.execute("INSERT OR IGNORE INTO bots(id,owner,name,settings,access,knowledge_pages,visibility) VALUES (?1,'owner','fixture',x'7b7d',x'7b7d',x'5b5d','private')",[key.bot.to_string()]).unwrap();
        self.db.connection.execute("INSERT OR IGNORE INTO threads(id,bot,kind,title,lifecycle) VALUES (?1,?2,'conversation','','active')",params![key.thread.to_string(),key.bot.to_string()]).unwrap();
        self.db.connection.execute("INSERT OR IGNORE INTO participants(thread,participant,kind) VALUES (?1,'person','person'),(?1,?2,'bot')",params![key.thread.to_string(),key.bot.to_string()]).unwrap();
        self.db
            .accept_message(
                &Person {
                    sender: "person".into(),
                    owner: "owner".into(),
                },
                &Envelope {
                    id: Uuid::new_v4(),
                    to: key.bot,
                    thread: Some(key.thread),
                    in_reply_to: None,
                    body: "work".into(),
                    attachments: vec![],
                    references: vec![],
                    deadline_ms: None,
                    reply_to: ReplyTo::Thread,
                },
                at,
            )
            .unwrap();
    }
    fn next(&mut self, at: u64) -> Option<Decision> {
        self.admission.next(&self.db, at).unwrap()
    }
    fn start(&mut self, key: SessionKey, at: u64) -> Reservation {
        let Some(Decision::Start {
            reservation,
            reuse: false,
        }) = self.next(at)
        else {
            panic!("new session expected")
        };
        assert_eq!(reservation.key, key);
        self.admission.bind_worker(key.bot, self.worker);
        self.admission.started(reservation, self.worker).unwrap();
        reservation
    }
    fn finish(&mut self, reservation: Reservation, at: i64) -> Delivery {
        self.admission
            .bind_provider(reservation, self.worker, reservation.session)
            .unwrap();
        let key = reservation.key;
        // The publication path requires the Bot's main conversation for cards.
        self.db
            .connection
            .execute(
                "UPDATE threads SET kind='main' WHERE id=?1",
                [key.thread.to_string()],
            )
            .unwrap();
        let delivery = self
            .db
            .claim_messages(
                key.bot,
                key.thread,
                self.worker,
                reservation.session,
                Resume::Rebuilt,
                65536,
                at,
            )
            .unwrap()
            .unwrap();
        self.db
            .delivery_accepted(delivery.turn, delivery.id)
            .unwrap();
        self.db
            .publish_reply(
                &Publish {
                    command: Uuid::new_v4(),
                    turn: delivery.turn,
                    body: "reply".into(),
                    outputs: vec![],
                },
                at,
            )
            .unwrap();
        delivery
    }
}

#[test]
fn one_record_counts_reservations_and_retains_stops_until_confirmed_death() {
    let mut f = Fixture::new(1);
    let a = f.key(1, 11);
    let b = f.key(2, 22);
    f.send(a, 1);
    let Some(Decision::Start {
        reservation,
        reuse: false,
    }) = f.next(1)
    else {
        panic!()
    };
    f.send(b, 2);
    assert_eq!(f.admission.occupied(), 1);
    assert_eq!(
        f.next(2),
        None,
        "reserved startup already consumes capacity"
    );
    f.admission.bind_worker(a.bot, f.worker);
    assert_eq!(f.admission.occupied_by(a.bot, f.worker), 1);
    f.admission.started(reservation, f.worker).unwrap();
    f.finish(reservation, 3);
    f.admission
        .idle(reservation, f.worker, 3, true, true)
        .unwrap();
    assert_eq!(f.next(4), Some(Decision::End(reservation)));
    assert_eq!(f.admission.occupied(), 1);
    assert_eq!(f.next(5), None);
    assert_eq!(
        f.admission.retired(reservation, Uuid::new_v4()),
        Err(Error::StaleTurn)
    );
    f.admission.retired(reservation, f.worker).unwrap();
    assert_eq!(f.start(b, 6).key, b);
}

#[test]
fn blocked_head_does_not_hide_a_turn_reusing_its_own_idle_session() {
    let mut f = Fixture::new(1);
    let a = f.key(1, 11);
    let b = f.key(2, 22);
    f.send(a, 1);
    let held = f.start(a, 1);
    f.finish(held, 2);
    f.admission.idle(held, f.worker, 2, false, false).unwrap();
    f.send(b, 3);
    f.send(a, 4);
    let Some(Decision::Start {
        reservation,
        reuse: true,
    }) = f.next(4)
    else {
        panic!("reuse expected")
    };
    assert_eq!(reservation.session, held.session);
    assert_ne!(reservation.assignment, held.assignment);
    assert_eq!(
        f.admission.idle(held, f.worker, 5, true, true),
        Err(Error::StaleTurn)
    );
    assert_eq!(f.admission.occupied(), 1);
}

#[test]
fn idle_blockers_are_read_again_after_the_delivery_settles() {
    let mut f = Fixture::new(1);
    let a = f.key(1, 11);
    f.send(a, 1);
    let held = f.start(a, 1);
    let delivered = f.finish(held, 2);
    f.db.connection
        .execute(
            "UPDATE deliveries SET state='unknown' WHERE id=?1",
            [delivered.id.to_string()],
        )
        .unwrap();
    f.admission.idle(held, f.worker, 2, true, true).unwrap();
    let b = f.key(2, 22);
    f.send(b, 3);
    assert_eq!(f.next(4), None);
    f.db.delivery_accepted(delivered.turn, delivered.id)
        .unwrap();
    assert_eq!(
        f.next(5),
        Some(Decision::End(held)),
        "no new idle snapshot is necessary"
    );
}

#[test]
fn fairness_and_pause_use_durable_work_and_capacity_reduction_preserves_live_work() {
    let mut f = Fixture::new(2);
    let a = f.key(1, 11);
    let a2 = f.key(1, 12);
    let b = f.key(2, 22);
    f.send(a, 1);
    let first = f.start(a, 1);
    f.send(a2, 2);
    f.send(b, 3);
    let second = f.start(b, 3);
    f.admission.set_capacity(1);
    assert_eq!(f.next(4), None);
    assert_eq!(f.admission.occupied(), 2);
    f.admission.worker_ended(f.worker);
    assert_eq!(f.admission.occupied(), 0);
    assert!(!f.admission.owns(first, f.worker));
    assert!(!f.admission.owns(second, f.worker));
    f.db.connection
        .execute("INSERT INTO team(owner,paused) VALUES ('owner',1)", [])
        .unwrap();
    assert_eq!(f.next(5), None);
    f.db.connection
        .execute("UPDATE team SET paused=0", [])
        .unwrap();
    assert!(f.next(6).is_some());
}

#[test]
fn a_late_retirement_cannot_release_a_replacement_session() {
    let mut f = Fixture::new(1);
    let a = f.key(1, 11);
    f.send(a, 1);
    let old = f.start(a, 1);
    f.admission.retired(old, f.worker).unwrap();
    let new = f.start(a, 2);
    assert_ne!(old.session, new.session);
    assert_eq!(f.admission.retired(old, f.worker), Err(Error::StaleTurn));
    assert_eq!(f.admission.occupied(), 1);
    assert!(f.admission.owns(new, f.worker));
}

#[test]
fn people_precede_routines_and_only_resumable_side_sessions_yield_early() {
    let mut f = Fixture::new(1);
    let routine = f.key(1, 11);
    let person = f.key(2, 22);
    f.send(routine, 1);
    f.db.connection
        .execute(
            "UPDATE threads SET kind='routine' WHERE id=?1",
            [routine.thread.to_string()],
        )
        .unwrap();
    f.send(person, 2);
    let held = f.start(person, 3);
    f.finish(held, 4);
    f.admission.idle(held, f.worker, 4, false, false).unwrap();
    assert_eq!(
        f.next(5),
        None,
        "pressure cannot discard unresumable context early"
    );
    assert_eq!(f.next(4 + IDLE_WINDOW_MS), Some(Decision::End(held)));
    f.admission.retired(held, f.worker).unwrap();
    let held = f.start(routine, 5 + IDLE_WINDOW_MS);
    f.finish(held, 6 + IDLE_WINDOW_MS as i64);
    f.db.connection
        .execute(
            "UPDATE threads SET kind='routine' WHERE id=?1",
            [routine.thread.to_string()],
        )
        .unwrap();
    f.admission
        .idle(held, f.worker, 6 + IDLE_WINDOW_MS, true, true)
        .unwrap();
    assert_eq!(f.next(7 + IDLE_WINDOW_MS), Some(Decision::End(held)));
}
