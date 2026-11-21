// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::{
    authority::{Mint, Worker},
    person,
};
use koru_time_zone::SystemZone;
mod conversation;
mod review;
mod startup;
mod tools;

#[test]
fn history_projects_needs_you_only_for_the_exact_current_delivered_occurrence() {
    let mut f = Fixture::new();
    let row = f.create();
    let old = f
        .change(
            Change::RunNow {
                bot: f.bot,
                routine: row.id,
                revision: 1,
            },
            10,
        )
        .occurrence
        .unwrap();
    let delivery = f.claim(&row, 11);
    f.publish(&delivery, 12);
    let current = f
        .change(
            Change::RunNow {
                bot: f.bot,
                routine: row.id,
                revision: 1,
            },
            20,
        )
        .occurrence
        .unwrap();
    let delivery = f.claim(&row, 21);
    f.db.connection
        .execute(
            "UPDATE turns SET state='needs_you' WHERE id=?1",
            [delivery.turn.id.to_string()],
        )
        .unwrap();
    let history = f.history(&row);
    assert_eq!(
        history.iter().find(|o| o.id == old.id).unwrap().state,
        "answered"
    );
    assert_eq!(
        history.iter().find(|o| o.id == current.id).unwrap().state,
        "needs_you"
    );
    let stored: String =
        f.db.connection
            .query_row(
                "SELECT state FROM routine_occurrences WHERE id=?1",
                [current.id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(stored, "running");
    f.db.connection
        .execute(
            "UPDATE turns SET state='running' WHERE id=?1",
            [delivery.turn.id.to_string()],
        )
        .unwrap();
    assert_eq!(
        f.history(&row)
            .iter()
            .find(|o| o.id == current.id)
            .unwrap()
            .state,
        "running"
    );
}

#[test]
fn deep_routine_history_retains_each_exact_reply_and_equal_time_cursor() {
    let mut f = Fixture::new();
    let row = f.create();
    let mut expected = std::collections::HashMap::new();
    for index in 0..19 {
        let result = f.change(
            Change::RunNow {
                bot: f.bot,
                routine: row.id,
                revision: 1,
            },
            10,
        );
        let occurrence = result.occurrence.unwrap();
        let delivery = f.claim(&row, 11);
        let body = format!("Historical reply {index}");
        f.db.publish_reply(
            &messages::Publish {
                command: Uuid::new_v4(),
                turn: delivery.turn,
                body: body.clone(),
                outputs: vec![],
            },
            12,
        )
        .unwrap();
        expected.insert(occurrence.id, body);
    }
    let first = f.db.routine(&f.who, f.bot, row.id, None, 16).unwrap();
    assert_eq!(first.occurrences.len(), 16);
    let older = f.db.routine(&f.who, f.bot, row.id, first.next, 16).unwrap();
    assert_eq!(older.occurrences.len(), 3);
    let mut seen = std::collections::HashSet::new();
    for occurrence in first.occurrences.into_iter().chain(older.occurrences) {
        assert!(seen.insert(occurrence.id));
        let preview = occurrence.preview.unwrap();
        assert_eq!(Some(preview.message.as_uuid()), occurrence.reply);
        assert_eq!(preview.markdown, expected[&occurrence.id]);
    }
}

#[test]
fn human_message_in_routine_thread_precedes_an_older_scheduled_message() {
    let mut f = Fixture::new();
    let automatic = f.create();
    let human = f.create();
    f.change(
        Change::RunNow {
            bot: f.bot,
            routine: automatic.id,
            revision: 1,
        },
        10,
    );
    f.db.accept_message(
        &f.who,
        &messages::Envelope {
            id: Uuid::new_v4(),
            to: f.bot,
            thread: Some(human.thread),
            body: "Please explain this Routine".into(),
            in_reply_to: None,
            attachments: vec![],
            references: vec![],
            deadline_ms: None,
            reply_to: messages::ReplyTo::Thread,
        },
        11,
    )
    .unwrap();
    let mut admission = crate::capacity::Admission::new(1);
    let Some(crate::capacity::Decision::Start { reservation, .. }) =
        admission.next(&f.db, 12).unwrap()
    else {
        panic!("human work must admit")
    };
    assert_eq!(reservation.key.thread, human.thread);
}

#[test]
fn expired_queue_outside_settlement_page_does_not_skip_a_current_firing() {
    let mut f = Fixture::new();
    let mut rows = (0..33)
        .map(|_| {
            let row = f.create();
            f.enable(&row)
        })
        .collect::<Vec<_>>();
    rows.sort_by_key(|row| row.id);
    for _ in 0..3 {
        f.tick(60_000);
    }
    // Reverse the durable occurrence ordering relative to definition ordering.
    // The first settlement page cannot include the first due definitions.
    for (index, row) in rows.iter().enumerate() {
        let changed = f.db.connection
            .execute(
                "UPDATE routine_occurrences SET id=?1 WHERE routine=?2 AND occurrence_ms=60000 AND manual=0",
                params![
                    Uuid::from_u128((100 - index) as u128).to_string(),
                    row.id.to_string()
                ],
            )
            .unwrap();
        assert_eq!(
            changed, 1,
            "only the exact queued minute gets the synthetic ID"
        );
    }
    for _ in 0..3 {
        f.tick(180_000);
    }
    for row in rows {
        assert!(
            f.history(&row)
                .iter()
                .any(|entry| entry.occurrence_ms == 180_000 && entry.state == "queued")
        );
    }
}

pub(crate) struct Fixture {
    pub(crate) db: Database,
    pub(crate) who: Person,
    pub(crate) bot: Uuid,
    pub(crate) main: Uuid,
}
impl Fixture {
    pub(crate) fn new() -> Self {
        Self::with_db(Database::in_memory().unwrap())
    }
    fn with_db(mut db: Database) -> Self {
        let who = Person {
            owner: "owner".into(),
            sender: "person".into(),
        };
        let bot = Uuid::new_v4();
        let home = koru_delegate_core::WorkspaceId::new();
        let definition = person::Definition {
            name: "Scout".into(),
            avatar: None,
            configuration: crate::configuration::Configuration {
                provider: "claude-acp".parse().unwrap(),
                selection: Default::default(),
                working_folder: None,
            },
            access: CapturedBotPolicy {
                workspace: home,
                destination: home,
                access: BotAccess::Workspace,
                selected_pages: Default::default(),
                connections: Default::default(),
            },
            role: None,
            knowledge: vec![],
            memory: None,
        };
        let reply = person::handle(
            &mut db,
            &Default::default(),
            &mut Default::default(),
            person::Request {
                owner: who.owner.clone(),
                sender: who.sender.clone(),
                command: person::Command::Change {
                    id: Uuid::new_v4(),
                    action: person::Change::Create { bot, definition },
                },
            },
            0,
        )
        .unwrap();
        let created: Receipt<person::Bot> = serde_json::from_value(reply.0).unwrap();
        Self {
            db,
            who,
            bot,
            main: created.answer.main_thread,
        }
    }
    pub(crate) fn create(&mut self) -> Routine {
        // Most cases begin before their first due minute, not exactly on a
        // boundary. Creation is now enabled, so an inclusive boundary matters.
        self.create_at(1)
    }
    fn create_at(&mut self, now: i64) -> Routine {
        self.change(
            Change::Create {
                bot: self.bot,
                expected_bot_revision: 1,
                name: "Briefing".into(),
                objective: "Summarize useful information".into(),
                recurrence: RecurrenceRule::zoned("* * * * *".parse().unwrap(), "UTC").unwrap(),
                documents: vec![],
            },
            now,
        )
        .routine
    }
    pub(crate) fn change(&mut self, action: Change, now: i64) -> Changed {
        change(&mut self.db, &self.who, Uuid::new_v4(), &action, now)
            .unwrap()
            .answer
    }
    fn enable(&mut self, row: &Routine) -> Routine {
        self.change(
            Change::Enable {
                bot: self.bot,
                routine: row.id,
                revision: row.revision,
                enabled: true,
            },
            1,
        )
        .routine
    }
    fn tick(&mut self, now: i64) {
        self.db
            .evaluate_routines(now, &SystemZone::utc(), None)
            .unwrap();
    }
    fn history(&self, row: &Routine) -> Vec<Occurrence> {
        self.db
            .routine(&self.who, self.bot, row.id, None, 16)
            .unwrap()
            .occurrences
    }
    pub(crate) fn claim(&mut self, row: &Routine, now: i64) -> messages::Delivery {
        let delivery = self
            .db
            .claim_messages(
                self.bot,
                row.thread,
                Uuid::new_v4(),
                Uuid::new_v4(),
                messages::Resume::Rebuilt,
                128 * 1024,
                now,
            )
            .unwrap()
            .unwrap();
        self.db
            .delivery_accepted(delivery.turn, delivery.id)
            .unwrap();
        delivery
    }
    pub(crate) fn publish(&mut self, delivery: &messages::Delivery, now: i64) -> messages::Publish {
        let request = messages::Publish {
            command: Uuid::new_v4(),
            turn: delivery.turn,
            body: "Result".into(),
            outputs: vec![],
        };
        self.db.publish_reply(&request, now).unwrap();
        request
    }
}

#[test]
fn created_on_separate_access_and_receipt_replay() {
    let mut f = Fixture::new();
    let row = f.create();
    assert!(row.enabled);
    assert_eq!(row.access.access, BotAccess::Default);
    f.tick(60_000);
    assert_eq!(f.history(&row).len(), 1);
    let command = Uuid::new_v4();
    let action = Change::Enable {
        bot: f.bot,
        routine: row.id,
        revision: 1,
        enabled: true,
    };
    let first = change(&mut f.db, &f.who, command, &action, 1).unwrap();
    assert!(
        !first.answer.first_enabled,
        "creation offers startup separately"
    );
    assert!(
        change(&mut f.db, &f.who, command, &action, 2)
            .unwrap()
            .replayed
    );
    let other = Person {
        owner: "other".into(),
        sender: "person".into(),
    };
    assert!(f.db.routines(&other, f.bot, None, 16).is_err());
    assert!(f.db.routine(&other, f.bot, row.id, None, 16).is_err());
}

#[test]
fn created_on_at_due_boundary_retains_the_miss_without_running_it_late() {
    let mut f = Fixture::new();
    let row = f.create_at(0);
    assert_eq!(row.next_due_ms, Some(0));
    f.tick(60_000);
    let history = f.history(&row);
    assert_eq!(history.len(), 2);
    assert!(history.iter().any(|entry| entry.occurrence_ms == 0
        && entry.state == "missed"
        && entry.message.is_none()));
    assert!(history.iter().any(|entry| entry.occurrence_ms == 60_000
        && entry.state == "queued"
        && entry.message.is_some()));
    f.tick(60_001);
    assert_eq!(
        f.history(&row).len(),
        2,
        "the boundary cannot duplicate either occurrence"
    );
}
#[test]
fn three_due_routines_create_three_messages_and_cards_atomically() {
    let mut f = Fixture::new();
    let rows = (0..3)
        .map(|_| {
            let r = f.create();
            f.enable(&r)
        })
        .collect::<Vec<_>>();
    f.tick(60_000);
    for row in &rows {
        let delivery = f.claim(row, 60_001);
        let published = f.publish(&delivery, 60_002);
        assert!(f.db.publish_reply(&published, 60_003).unwrap().replayed);
        let history = f.history(row);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].state, "answered");
        assert!(history[0].reply.is_some() && history[0].card.is_some());
    }
    assert_eq!(
        f.db.connection
            .query_row(
                "SELECT count(*) FROM messages WHERE thread=?1 AND kind='thread_reference'",
                [f.main.to_string()],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        3
    );
}
#[test]
fn every_firing_gets_a_card_in_same_routine_thread() {
    let mut f = Fixture::new();
    let row = f.create();
    let row = f.enable(&row);
    for now in [60_000, 120_000] {
        f.tick(now);
        let d = f.claim(&row, now + 1);
        f.publish(&d, now + 2);
    }
    let history = f.history(&row);
    assert_eq!(history.len(), 2);
    assert_ne!(history[0].card, history[1].card);
}
#[test]
fn capacity_deadline_is_exclusive_and_never_catches_up() {
    let mut f = Fixture::new();
    let row = f.create();
    let row = f.enable(&row);
    f.tick(60_000);
    assert!(
        f.db.claim_messages(
            f.bot,
            row.thread,
            Uuid::new_v4(),
            Uuid::new_v4(),
            messages::Resume::Rebuilt,
            128 * 1024,
            120_000
        )
        .unwrap()
        .is_none()
    );
    f.tick(180_001);
    let history = f.history(&row);
    assert_eq!(
        history
            .iter()
            .filter(|r| r.occurrence_ms == 60_000 && r.state == "missed")
            .count(),
        1
    );
    // Only the currently due minute may be queued, never the old occurrence.
    assert!(
        history
            .iter()
            .all(|r| r.state != "queued" || r.occurrence_ms == 180_000)
    );
}
#[test]
fn restart_sleep_and_clock_rollback_do_not_duplicate_occurrences() {
    let mut f = Fixture::new();
    let row = f.create();
    let row = f.enable(&row);
    f.tick(60_000);
    f.db.evaluate_routines(60_002, &SystemZone::utc(), Some(None))
        .unwrap();
    assert_eq!(f.history(&row).len(), 1);
    f.db.evaluate_routines(1, &SystemZone::utc(), Some(None))
        .unwrap();
    f.tick(60_000);
    assert_eq!(f.history(&row).len(), 1);
    f.db.evaluate_routines(31 * 24 * 60 * 60_000 + 1, &SystemZone::utc(), Some(None))
        .unwrap();
    assert!(f.history(&row).len() <= 3);
}
#[test]
fn busy_routine_is_skipped_but_other_routine_is_admitted() {
    let mut f = Fixture::new();
    let a = f.create();
    let a = f.enable(&a);
    let b = f.create();
    let b = f.enable(&b);
    f.tick(60_000);
    let running = f.claim(&a, 60_001);
    let other = f.claim(&b, 60_001);
    f.publish(&other, 60_002);
    f.tick(120_000);
    assert!(
        f.history(&a)
            .iter()
            .any(|r| r.occurrence_ms == 120_000 && r.state == "skipped")
    );
    assert!(
        f.history(&b)
            .iter()
            .any(|r| r.occurrence_ms == 120_000 && r.state == "queued")
    );
    f.publish(&running, 120_002);
}
#[test]
fn run_now_does_not_enable_or_share_scheduled_deadline() {
    let mut f = Fixture::new();
    let row = f.create();
    let row = f
        .change(
            Change::Enable {
                bot: f.bot,
                routine: row.id,
                revision: row.revision,
                enabled: false,
            },
            1,
        )
        .routine;
    let command = Uuid::new_v4();
    let action = Change::RunNow {
        bot: f.bot,
        routine: row.id,
        revision: row.revision,
    };
    let first = change(&mut f.db, &f.who, command, &action, 12).unwrap();
    assert!(!first.answer.routine.enabled);
    assert!(first.answer.occurrence.as_ref().unwrap().manual);
    assert!(
        change(&mut f.db, &f.who, command, &action, 13)
            .unwrap()
            .replayed
    );
    f.tick(600_000);
    assert_eq!(f.history(&row)[0].state, "queued");
    let d = f.claim(&row, 600_001);
    f.publish(&d, 600_002);
    assert_eq!(f.history(&row).len(), 1);
}
#[test]
fn pause_cancels_only_unclaimed_work_and_preserves_running_reply() {
    let mut f = Fixture::new();
    let a = f.create();
    let a = f.enable(&a);
    let b = f.create();
    let b = f.enable(&b);
    f.tick(60_000);
    let d = f.claim(&a, 60_001);
    for row in [&a, &b] {
        f.change(
            Change::Enable {
                bot: f.bot,
                routine: row.id,
                revision: row.revision,
                enabled: false,
            },
            60_002,
        );
    }
    assert_eq!(f.history(&b)[0].state, "cancelled");
    assert_eq!(f.history(&a)[0].state, "running");
    f.publish(&d, 60_003);
    assert_eq!(f.history(&a)[0].state, "answered");
}
#[test]
fn routine_policy_captures_session_and_turn_and_narrows_next_call() {
    let mut f = Fixture::new();
    let row = f.create();
    let mut initial = row.access.clone();
    initial
        .selected_pages
        .insert(koru_delegate_core::DocumentId::new());
    let row = f
        .change(
            Change::Access {
                bot: f.bot,
                routine: row.id,
                revision: 1,
                access: initial,
            },
            1,
        )
        .routine;
    f.change(
        Change::RunNow {
            bot: f.bot,
            routine: row.id,
            revision: 1,
        },
        10,
    );
    let worker = Worker {
        bot: f.bot,
        incarnation: Uuid::new_v4(),
    };
    f.db.connection
        .execute(
            "INSERT INTO workers VALUES (?1,?2,0,0)",
            params![f.bot.to_string(), worker.incarnation.to_string()],
        )
        .unwrap();
    let issued =
        f.db.mint_session(
            worker,
            Mint {
                command: Uuid::new_v4(),
                thread: row.thread,
                predecessor: None,
            },
            11,
        )
        .unwrap();
    let secret = issued.secret.unwrap();
    let captured: CapturedBotPolicy =
        serde_json::from_value(f.db.session_catalog_grant(worker, &secret).unwrap()).unwrap();
    assert_eq!(captured, row.access);
    let delivery =
        f.db.claim_session_messages(
            worker,
            issued.session,
            messages::Resume::Rebuilt,
            128 * 1024,
            12,
        )
        .unwrap()
        .unwrap();
    let call =
        crate::authority::Call::parse(Uuid::new_v4(), "read_document".into(), b"{}").unwrap();
    let binding =
        f.db.bind_call(worker, &secret, &call, Uuid::new_v4(), 13)
            .unwrap();
    let resolved =
        f.db.resolve_descriptor(&secret, binding.id, call.descriptor())
            .unwrap();
    assert_eq!(
        resolved.captured_grant,
        serde_json::to_value(&row.access).unwrap()
    );
    let mut access = row.access.clone();
    access.selected_pages.clear();
    f.change(
        Change::Access {
            bot: f.bot,
            routine: row.id,
            revision: row.grant_revision,
            access: access.clone(),
        },
        14,
    );
    let resolved =
        f.db.resolve_descriptor(&secret, binding.id, call.descriptor())
            .unwrap();
    assert_eq!(
        resolved.current_grant,
        serde_json::to_value(&access).unwrap()
    );
    assert_ne!(resolved.current_grant, resolved.captured_grant);
    assert_eq!(resolved.binding.turn, delivery.turn);
}
#[test]
fn invited_bot_uses_its_policy_and_missing_owner_routine_is_refused() {
    let mut f = Fixture::new();
    let row = f.create();
    let guest = Uuid::new_v4();
    f.db.connection.execute("INSERT INTO bots(id,owner,name,settings,access,knowledge_pages,visibility) SELECT ?1,owner,'guest',settings,access,knowledge_pages,visibility FROM bots WHERE id=?2",params![guest.to_string(),f.bot.to_string()]).unwrap();
    f.db.connection
        .execute(
            "INSERT INTO participants(thread,participant,kind) VALUES (?1,?2,'bot')",
            params![row.thread.to_string(), guest.to_string()],
        )
        .unwrap();
    let guest_grant: CapturedBotPolicy =
        decode(&grant(&f.db.connection, guest, row.thread).unwrap()).unwrap();
    assert_eq!(guest_grant.access, BotAccess::Workspace);
    f.db.connection
        .execute("DELETE FROM routines WHERE id=?1", [row.id.to_string()])
        .unwrap();
    assert!(grant(&f.db.connection, f.bot, row.thread).is_err());
}
#[test]
fn failed_publication_rolls_back_card_and_occurrence() {
    let mut f = Fixture::new();
    let row = f.create();
    f.change(
        Change::RunNow {
            bot: f.bot,
            routine: row.id,
            revision: 1,
        },
        10,
    );
    let delivery = f.claim(&row, 11);
    f.db.connection.execute_batch("CREATE TEMP TRIGGER refuse_card BEFORE INSERT ON messages WHEN NEW.kind='thread_reference' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    let request = messages::Publish {
        command: Uuid::new_v4(),
        turn: delivery.turn,
        body: "Result".into(),
        outputs: vec![],
    };
    assert!(f.db.publish_reply(&request, 12).is_err());
    assert!(f.history(&row)[0].card.is_none());
    assert_eq!(f.history(&row)[0].state, "running");
    f.db.connection
        .execute_batch("DROP TRIGGER refuse_card;")
        .unwrap();
    f.db.publish_reply(&request, 13).unwrap();
    assert!(f.history(&row)[0].card.is_some());
}

#[test]
fn routine_access_cannot_change_output_workspace_or_home() {
    let mut f = Fixture::new();
    let row = f.create();
    let mut access = row.access.clone();
    access.destination = koru_delegate_core::WorkspaceId::new();
    assert!(
        change(
            &mut f.db,
            &f.who,
            Uuid::new_v4(),
            &Change::Access {
                bot: f.bot,
                routine: row.id,
                revision: 1,
                access: access.clone()
            },
            10
        )
        .is_err()
    );
    access.workspace = access.destination;
    assert!(
        change(
            &mut f.db,
            &f.who,
            Uuid::new_v4(),
            &Change::Access {
                bot: f.bot,
                routine: row.id,
                revision: 1,
                access
            },
            10
        )
        .is_err()
    );
}

#[test]
fn normal_admission_starts_three_routines_and_prioritizes_human_work() {
    use crate::capacity::{Admission, Decision};
    let mut f = Fixture::new();
    let rows = (0..3)
        .map(|_| {
            let row = f.create();
            f.enable(&row)
        })
        .collect::<Vec<_>>();
    f.tick(60_000);
    let mut admission = Admission::new(3);
    let mut threads = std::collections::BTreeSet::new();
    for _ in 0..3 {
        let Some(Decision::Start { reservation, .. }) = admission.next(&f.db, 60_001).unwrap()
        else {
            panic!("three independent routine threads must admit together")
        };
        threads.insert(reservation.key.thread);
    }
    assert_eq!(threads, rows.iter().map(|row| row.thread).collect());
    let mut admission = Admission::new(1);
    f.db.accept_message(
        &f.who,
        &messages::Envelope {
            id: Uuid::new_v4(),
            to: f.bot,
            thread: Some(f.main),
            in_reply_to: None,
            body: "Person first".into(),
            attachments: vec![],
            references: vec![],
            deadline_ms: None,
            reply_to: Default::default(),
        },
        60_002,
    )
    .unwrap();
    let Some(Decision::Start { reservation, .. }) = admission.next(&f.db, 60_003).unwrap() else {
        panic!()
    };
    assert_eq!(reservation.key.thread, f.main);
    assert!(admission.next(&f.db, 60_003).unwrap().is_none());
}

#[test]
fn cards_name_the_exact_occurrence_and_its_own_reply() {
    let mut f = Fixture::new();
    let row = f.create();
    let row = f.enable(&row);
    for now in [60_000, 120_000] {
        f.tick(now);
        let d = f.claim(&row, now + 1);
        f.publish(&d, now + 2);
    }
    let response = person::handle(
        &mut f.db,
        &Default::default(),
        &mut Default::default(),
        person::Request {
            owner: f.who.owner.clone(),
            sender: f.who.sender.clone(),
            command: person::Command::Read {
                query: person::Query::Cards {
                    thread: f.main,
                    after: None,
                },
            },
        },
        120_003,
    )
    .unwrap();
    let items = response.0["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    for item in items {
        let fact = &item["routine"];
        assert_eq!(fact["routine"], row.id.to_string());
        assert_eq!(fact["preview"]["markdown"], "Result");
        let found = f
            .history(&row)
            .into_iter()
            .find(|entry| serde_json::json!(entry.id) == fact["occurrence"])
            .unwrap();
        assert_eq!(serde_json::json!(found.reply), fact["reply"]);
    }
}
