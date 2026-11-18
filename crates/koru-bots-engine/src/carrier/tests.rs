// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::{
    capacity::{Decision, SessionKey},
    messages::{Envelope, Person, ReplyTo},
};

struct Fixture {
    db: Database,
    admission: Admission,
    who: Worker,
    reservation: Reservation,
}

#[test]
fn door_requires_current_assignment_and_bound_history_cannot_choose_another_thread() {
    let mut f = Fixture::new();
    let issued = f.mint(None);
    let secret = issued.secret.unwrap();
    let session = issued.session;
    assert!(matches!(
        f.request(Command::Catalog {
            reservation: f.reservation,
            session,
            secret: secret.clone()
        }),
        Reply::CatalogGrant(_)
    ));
    let call = authority::Call::parse(
        Uuid::new_v4(),
        "read_older_messages".into(),
        br#"{"after":0}"#,
    )
    .unwrap();
    let bind = |reservation| Command::Bind {
        reservation,
        session,
        secret: secret.clone(),
        command: Uuid::new_v4(),
        descriptor: call.descriptor().clone(),
    };
    assert!(matches!(
        f.request(bind(f.reservation)),
        Reply::Refused(Error::NoTurn)
    ));
    let Reply::Delivery(Some(delivery)) = f.request(Command::Claim {
        reservation: f.reservation,
        session,
        resume: Resume::Rebuilt,
        budget: 65536,
    }) else {
        panic!()
    };
    let Reply::Bound(binding) = f.request(bind(f.reservation)) else {
        panic!()
    };
    let own = |reservation, arguments: &[u8]| Command::OwnTool {
        reservation,
        session,
        secret: secret.clone(),
        binding: binding.id,
        door_call: call.door_call(),
        tool: call.tool().into(),
        arguments: arguments.to_vec(),
    };
    let mut wrong = f.reservation;
    wrong.assignment = Uuid::new_v4();
    assert!(matches!(
        f.request(own(wrong, br#"{"after":0}"#)),
        Reply::Refused(Error::StaleTurn)
    ));
    assert!(matches!(
        f.request(own(f.reservation, br#"{"after":1}"#)),
        Reply::Refused(Error::BindingConflict)
    ));
    let Reply::Tool(history) = f.request(own(f.reservation, br#"{"after":0}"#)) else {
        panic!()
    };
    assert_eq!(history["messages"].as_array().unwrap().len(), 1);
    assert_eq!(history["messages"][0]["content"]["body"], "synthetic");
    f.db.stop_turn(
        &Person {
            owner: "owner".into(),
            sender: "person".into(),
        },
        Uuid::new_v4(),
        delivery.turn,
        4,
    )
    .unwrap();
    assert!(matches!(
        f.request(own(f.reservation, br#"{"after":0}"#)),
        Reply::Refused(Error::CredentialRevoked)
    ));
}
impl Fixture {
    fn new() -> Self {
        let mut db = Database::in_memory().unwrap();
        let who = Worker {
            bot: Uuid::new_v4(),
            incarnation: Uuid::new_v4(),
        };
        let thread = Uuid::new_v4();
        db.connection.execute("INSERT INTO bots(id,owner,name,settings,access,knowledge_pages,visibility) VALUES (?1,'owner','fixture',x'7b7d',x'7b7d',x'5b5d','private')",[who.bot.to_string()]).unwrap();
        db.connection.execute("INSERT INTO threads(id,bot,kind,title,lifecycle) VALUES (?1,?2,'main','','active')",rusqlite::params![thread.to_string(),who.bot.to_string()]).unwrap();
        db.connection.execute("INSERT INTO participants(thread,participant,kind) VALUES (?1,'person','person'),(?1,?2,'bot')",rusqlite::params![thread.to_string(),who.bot.to_string()]).unwrap();
        db.connection
            .execute(
                "INSERT INTO workers VALUES (?1,?2,0,0)",
                rusqlite::params![who.bot.to_string(), who.incarnation.to_string()],
            )
            .unwrap();
        db.accept_message(
            &Person {
                owner: "owner".into(),
                sender: "person".into(),
            },
            &Envelope {
                id: Uuid::new_v4(),
                to: who.bot,
                thread: Some(thread),
                in_reply_to: None,
                body: "synthetic".into(),
                attachments: vec![],
                references: vec![],
                deadline_ms: None,
                reply_to: ReplyTo::Thread,
            },
            1,
        )
        .unwrap();
        let mut admission = Admission::new(1);
        let Some(Decision::Start { reservation, .. }) = admission.next(&db, 2).unwrap() else {
            panic!()
        };
        admission.bind_worker(who.bot, who.incarnation);
        Self {
            db,
            admission,
            who,
            reservation,
        }
    }
    fn request(&mut self, command: Command) -> Reply {
        handle(&mut self.db, &mut self.admission, self.who, command, 3)
    }
    fn mint(&mut self, predecessor: Option<Uuid>) -> Issued {
        let Reply::Issued(issued) = self.request(Command::Mint {
            reservation: self.reservation,
            request: Mint {
                command: Uuid::new_v4(),
                thread: self.reservation.key.thread,
                predecessor,
            },
        }) else {
            panic!("mint failed")
        };
        issued
    }
}

#[test]
fn registered_worker_still_needs_the_exact_reserved_assignment() {
    let mut f = Fixture::new();
    let mut stale = f.reservation;
    stale.assignment = Uuid::new_v4();
    assert!(matches!(
        f.request(Command::Started { reservation: stale }),
        Reply::Refused(Error::StaleTurn)
    ));
    assert_eq!(f.admission.occupied(), 1);
    let Reply::Work { starts, stops } = f.request(Command::Poll) else {
        panic!()
    };
    assert_eq!(starts, vec![f.reservation]);
    assert!(stops.is_empty());
    let other = Worker {
        bot: Uuid::new_v4(),
        ..f.who
    };
    assert!(matches!(
        handle(
            &mut f.db,
            &mut f.admission,
            other,
            Command::Started {
                reservation: f.reservation
            },
            3
        ),
        Reply::Refused(Error::WorkerReplaced)
    ));
    assert_eq!(f.admission.occupied(), 1);
}

#[test]
fn failed_start_replacement_keeps_one_slot_and_claim_requires_the_current_credential() {
    let mut f = Fixture::new();
    assert!(matches!(
        f.request(Command::Started {
            reservation: f.reservation
        }),
        Reply::Done
    ));
    let first = f.mint(None);
    let second = f.mint(Some(first.session));
    assert_eq!(f.admission.occupied(), 1);
    assert_ne!(first.session, second.session);
    assert!(matches!(
        f.request(Command::Claim {
            reservation: f.reservation,
            session: first.session,
            resume: Resume::Rebuilt,
            budget: 65536
        }),
        Reply::Refused(Error::StaleTurn)
    ));
    let Reply::Delivery(Some(delivery)) = f.request(Command::Claim {
        reservation: f.reservation,
        session: second.session,
        resume: Resume::Rebuilt,
        budget: 65536,
    }) else {
        panic!()
    };
    let mut wrong = delivery.turn;
    wrong.worker = Uuid::new_v4();
    assert!(matches!(
        f.request(Command::Accepted {
            reservation: f.reservation,
            turn: wrong,
            delivery: delivery.id
        }),
        Reply::Refused(Error::StaleTurn)
    ));
    assert!(matches!(
        f.request(Command::Accepted {
            reservation: f.reservation,
            turn: delivery.turn,
            delivery: delivery.id
        }),
        Reply::Done
    ));
    assert!(matches!(
        f.request(Command::Retired {
            reservation: f.reservation,
            turn: Some(delivery.turn)
        }),
        Reply::Done
    ));
    assert_eq!(f.admission.occupied(), 0);
    assert_eq!(f.db.status().unwrap().running_turns, 0);
    assert_eq!(
        f.db.session_catalog_grant(f.who, &second.secret.unwrap())
            .err(),
        Some(Error::CredentialRevoked)
    );
}

#[test]
fn forged_thread_and_stale_retirement_cannot_touch_a_new_reservation() {
    let mut f = Fixture::new();
    let issued = f.mint(None);
    assert!(matches!(
        f.request(Command::Retired {
            reservation: f.reservation,
            turn: None
        }),
        Reply::Done
    ));
    let old = f.reservation;
    let Some(Decision::Start { reservation, .. }) = f.admission.next(&f.db, 4).unwrap() else {
        panic!()
    };
    f.admission.bind_worker(f.who.bot, f.who.incarnation);
    f.reservation = reservation;
    assert!(matches!(
        f.request(Command::Retired {
            reservation: old,
            turn: None
        }),
        Reply::Refused(Error::StaleTurn)
    ));
    assert_eq!(f.admission.occupied(), 1);
    let forged = Reservation {
        key: SessionKey {
            thread: Uuid::new_v4(),
            ..reservation.key
        },
        ..reservation
    };
    assert!(matches!(
        f.request(Command::Mint {
            reservation: forged,
            request: Mint {
                command: Uuid::new_v4(),
                thread: forged.key.thread,
                predecessor: Some(issued.session)
            }
        }),
        Reply::Refused(Error::StaleTurn)
    ));
}

#[test]
fn lost_claim_reply_does_not_strand_a_running_turn_after_confirmed_retirement() {
    let mut f = Fixture::new();
    let issued = f.mint(None);
    let _lost = f.request(Command::Claim {
        reservation: f.reservation,
        session: issued.session,
        resume: Resume::Rebuilt,
        budget: 65536,
    });
    assert_eq!(f.db.status().unwrap().running_turns, 1);
    assert!(matches!(
        f.request(Command::Retired {
            reservation: f.reservation,
            turn: None
        }),
        Reply::Done
    ));
    assert_eq!(f.db.status().unwrap().running_turns, 0);
    assert_eq!(f.admission.occupied(), 0);
    assert!(f.admission.next(&f.db, 4).unwrap().is_some());
}
