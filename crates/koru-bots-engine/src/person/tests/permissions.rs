// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn permissions_preserve_omitted_switch_and_exact_control_receipt_survives_answer_retention() {
    let mut f = Fixture::new();
    let request = Uuid::new_v4();
    let page = Uuid::new_v4();
    let action = Change::Permissions {
        bot: f.bot.id,
        revision: 1,
        policy_revision: 1,
        access: koru_delegate_core::BotAccess::Workspace,
        selected_pages: vec![page],
        connections: None,
        may_ask: Some(false),
    };
    let first = change::apply(&mut f.db, &f.who, request, &action, 2).unwrap();
    assert_eq!(first.answer["policy_revision"], 2);
    assert_eq!(first.answer["may_ask"], false);
    let later = f.change(Change::Permissions {
        bot: f.bot.id,
        revision: 2,
        policy_revision: 2,
        access: koru_delegate_core::BotAccess::Default,
        selected_pages: vec![],
        connections: Some(Default::default()),
        may_ask: None,
    });
    assert_eq!(later["may_ask"], false);
    assert_eq!(later["policy_revision"], 3);
    assert_eq!(
        change::apply(&mut f.db, &f.who, request, &action, 4)
            .unwrap()
            .answer,
        first.answer
    );
    let mut changed = action.clone();
    if let Change::Permissions { may_ask, .. } = &mut changed {
        *may_ask = None;
    }
    assert_eq!(
        change::apply(&mut f.db, &f.who, request, &changed, 4).err(),
        Some(Error::Conflict)
    );
    let read_control = |db: &Database, who: &Person| {
        read::query(
            db,
            who,
            Query::ControlReceipt {
                bot: f.bot.id,
                request,
            },
        )
    };
    assert_eq!(read_control(&f.db, &f.who).unwrap()["kind"], "access");
    f.db.prune_receipt_answers(messages::RETENTION_MS + 100)
        .unwrap();
    assert_eq!(read_control(&f.db, &f.who).unwrap()["kind"], "access");
    assert_eq!(
        change::apply(
            &mut f.db,
            &f.who,
            request,
            &action,
            messages::RETENTION_MS + 100
        )
        .err(),
        Some(Error::Expired)
    );
    let other = Person {
        owner: "other".into(),
        sender: f.who.sender.clone(),
    };
    assert_eq!(read_control(&f.db, &other).err(), Some(Error::NotAllowed));
}

#[test]
fn stale_policy_and_failed_write_leave_all_permission_fields_and_control_evidence_unchanged() {
    let mut f = Fixture::new();
    let request = Uuid::new_v4();
    let mut action = Change::Permissions {
        bot: f.bot.id,
        revision: 1,
        policy_revision: 2,
        access: koru_delegate_core::BotAccess::Workspace,
        selected_pages: vec![],
        connections: None,
        may_ask: Some(false),
    };
    assert_eq!(
        change::apply(&mut f.db, &f.who, request, &action, 2).err(),
        Some(Error::RevisionConflict)
    );
    if let Change::Permissions {
        policy_revision, ..
    } = &mut action
    {
        *policy_revision = 1;
    }
    f.db.connection.execute_batch("CREATE TRIGGER fail_access BEFORE UPDATE OF access ON bots BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert_eq!(
        change::apply(&mut f.db, &f.who, request, &action, 2).err(),
        Some(Error::StorageUnavailable)
    );
    let bot = read::bot(&f.db.connection, &f.who, f.bot.id).unwrap();
    assert_eq!(bot.revision, 1);
    assert_eq!(bot.policy_revision, 1);
    assert!(bot.may_ask);
    assert!(bot.definition.access.access == koru_delegate_core::BotAccess::Default);
    assert_eq!(
        read::query(
            &f.db,
            &f.who,
            Query::ControlReceipt {
                bot: f.bot.id,
                request
            }
        )
        .err(),
        Some(Error::NotAllowed)
    );
}
