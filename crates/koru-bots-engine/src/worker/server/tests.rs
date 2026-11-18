// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn authority_requires_the_whole_secret() {
    assert!(matches_authority("abcd", "abcd"));
    assert!(!matches_authority("abcd", "abce"));
    assert!(!matches_authority("abc", "abcd"));
}

#[test]
fn bootstrap_debug_contains_neither_root_nor_authority() {
    let bootstrap = Bootstrap {
        launch_id: Uuid::new_v4(),
        segment: "dev".into(),
        root: "private-root".into(),
        authority: "private-secret".into(),
    };
    let debug = format!("{bootstrap:?}");
    assert!(!debug.contains("private-root"));
    assert!(!debug.contains("private-secret"));
}

#[test]
fn failed_wake_is_a_refusal_and_next_command_can_succeed() {
    let db = Database::in_memory().unwrap();
    let bot = Uuid::new_v4();
    db.connection.execute("INSERT INTO bots(id,owner,name,settings,access,knowledge_pages,visibility) VALUES (?1,'owner','fixture',x'7b7d',x'7b7d',x'5b5d','private')",[bot.to_string()]).unwrap();
    db.connection
        .execute_batch("ALTER TABLE bots RENAME TO temporarily_unavailable;")
        .unwrap();
    assert!(matches!(
        wake_bot(&db, bot, || panic!("must not launch on failed lookup")),
        Reply::Refused(Error::StorageUnavailable)
    ));
    db.connection
        .execute_batch("ALTER TABLE temporarily_unavailable RENAME TO bots;")
        .unwrap();
    assert!(matches!(wake_bot(&db, bot, || Ok(())), Reply::Queued));
    db.connection
        .execute("INSERT INTO team VALUES ('owner',1)", [])
        .unwrap();
    assert!(matches!(
        wake_bot(&db, bot, || panic!("team paused")),
        Reply::Refused(Error::InvalidRequest)
    ));
}

#[test]
fn failed_worker_report_can_be_retried_without_losing_its_incarnation() {
    let mut db = Database::in_memory().unwrap();
    let bot = Uuid::new_v4();
    let incarnation = Uuid::new_v4();
    db.connection.execute("INSERT INTO bots(id,owner,name,settings,access,knowledge_pages,visibility) VALUES (?1,'owner','fixture',x'7b7d',x'7b7d',x'5b5d','private')",[bot.to_string()]).unwrap();
    let registered = crate::launcher::Event::Registered {
        bot,
        incarnation,
        at_ms: 1,
    };
    db.connection.execute_batch("CREATE TEMP TRIGGER reject_worker BEFORE INSERT ON workers BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(record_worker_event(&mut db, &registered).is_err());
    db.connection
        .execute_batch("DROP TRIGGER reject_worker;")
        .unwrap();
    record_worker_event(&mut db, &registered).unwrap();
    let ended = crate::launcher::Event::Ended { bot, incarnation };
    db.connection.execute_batch("CREATE TEMP TRIGGER reject_end BEFORE DELETE ON workers BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(record_worker_event(&mut db, &ended).is_err());
    assert_eq!(db.status().unwrap().awake_bots, 1);
    db.connection
        .execute_batch("DROP TRIGGER reject_end;")
        .unwrap();
    record_worker_event(&mut db, &ended).unwrap();
    assert_eq!(db.status().unwrap().awake_bots, 0);
}
