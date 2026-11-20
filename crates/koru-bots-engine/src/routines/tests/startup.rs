// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn startup_markers_survive_answer_retention_but_command_retries_still_expire() {
    let mut f = Fixture::new();
    f.create();
    let request = Uuid::new_v4();
    assert!(
        claim_startup(&mut f.db, &f.who, request, f.bot, 1)
            .unwrap()
            .answer
    );
    f.db.prune_receipt_answers(messages::RETENTION_MS + 100)
        .unwrap();
    assert_eq!(
        claim_startup(
            &mut f.db,
            &f.who,
            request,
            f.bot,
            messages::RETENTION_MS + 101
        )
        .err(),
        Some(Error::Expired)
    );
    assert!(
        !claim_startup(
            &mut f.db,
            &f.who,
            Uuid::new_v4(),
            f.bot,
            messages::RETENTION_MS + 102
        )
        .unwrap()
        .answer
    );
    assert_eq!(f.db.connection.query_row("SELECT count(*) FROM receipts WHERE sender LIKE 'routine-startup:%' AND answer IS NOT NULL",[],|r|r.get::<_,i64>(0)).unwrap(),2);
}

#[test]
fn first_created_on_offer_is_owner_wide_and_exact_retries_survive_new_callers() {
    let mut f = Fixture::new();
    let first = f.create();
    let second = f.create();
    let request = Uuid::new_v4();
    let got = claim_startup(&mut f.db, &f.who, request, f.bot, 10).unwrap();
    assert!(got.answer && !got.replayed);
    let another_pane = Person {
        owner: f.who.owner.clone(),
        sender: "second-window".into(),
    };
    assert!(
        !claim_startup(&mut f.db, &another_pane, Uuid::new_v4(), f.bot, 11)
            .unwrap()
            .answer
    );
    // No claimant state is retained outside SQLite: recreate the caller as
    // after a restart, and recover the same delivered receipt, not a new offer.
    let restarted = Person {
        owner: f.who.owner.clone(),
        sender: f.who.sender.clone(),
    };
    let replay = claim_startup(&mut f.db, &restarted, request, f.bot, 12).unwrap();
    assert!(replay.answer && replay.replayed);
    assert!(
        !claim_startup(&mut f.db, &restarted, Uuid::new_v4(), f.bot, 13)
            .unwrap()
            .answer
    );
    assert!(read(&f.db.connection, f.bot, first.id).unwrap().enabled);
    assert!(read(&f.db.connection, f.bot, second.id).unwrap().enabled);
}

#[test]
fn paused_candidate_does_not_offer_and_old_creation_receipt_never_enables_it() {
    let mut f = Fixture::new();
    let row = f.create();
    let paused = f
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
    assert!(
        !claim_startup(&mut f.db, &f.who, Uuid::new_v4(), f.bot, 2)
            .unwrap()
            .answer
    );
    let command = Uuid::new_v4();
    let action = Change::Create {
        bot: f.bot,
        expected_bot_revision: 1,
        name: row.name.clone(),
        objective: row.objective.clone(),
        recurrence: row.recurrence.clone(),
        documents: vec![],
    };
    let old_answer = Changed {
        routine: paused.clone(),
        occurrence: None,
        first_enabled: false,
    };
    messages::receipts::save(
        &f.db.connection,
        &f.who.receipt_subject(),
        command,
        &messages::receipts::digest("routine-change", &action).unwrap(),
        &old_answer,
        0,
    )
    .unwrap();
    let old = change(&mut f.db, &f.who, command, &action, 3).unwrap();
    assert!(old.replayed && !old.answer.routine.enabled);
    assert!(!read(&f.db.connection, f.bot, row.id).unwrap().enabled);
    let enabled = f.enable(&paused);
    assert!(enabled.enabled);
    assert!(
        claim_startup(&mut f.db, &f.who, Uuid::new_v4(), f.bot, 4)
            .unwrap()
            .answer
    );
}

#[test]
fn startup_claim_requires_the_owner_and_never_changes_a_schedule_or_grant() {
    let mut f = Fixture::new();
    let row = f.create();
    let other = Person {
        owner: "other-owner".into(),
        sender: "person".into(),
    };
    assert_eq!(
        claim_startup(&mut f.db, &other, Uuid::new_v4(), f.bot, 1).err(),
        Some(Error::NotAllowed)
    );
    let before = serde_json::to_value(&row).unwrap();
    claim_startup(&mut f.db, &f.who, Uuid::new_v4(), f.bot, 2).unwrap();
    assert_eq!(
        before,
        serde_json::to_value(read(&f.db.connection, f.bot, row.id).unwrap()).unwrap()
    );
}
