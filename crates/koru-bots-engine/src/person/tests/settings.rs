// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::configuration::{self, ConversationSettings};

fn saved(f: &Fixture, thread: Uuid) -> ConversationSettings {
    configuration::conversation(&f.db.connection, f.bot.id, thread).unwrap()
}
fn side(f: &mut Fixture) -> Thread {
    serde_json::from_value(f.change(Change::Conversation {
        bot: f.bot.id,
        title: "Side".into(),
    }))
    .unwrap()
}

#[test]
fn conversation_models_are_independent_and_new_chats_capture_updated_bot_defaults() {
    let mut f = Fixture::new();
    let main = f.bot.main_thread;
    let other = side(&mut f).id;
    let mut selection = saved(&f, main).configuration.selection;
    selection.model = Some("explicit-model".parse().unwrap());
    selection.fast = Some(true);
    let action = Change::ConversationSettings {
        bot: f.bot.id,
        thread: main,
        revision: 1,
        provider: f.bot.definition.configuration.provider.clone(),
        selection,
    };
    let id = Uuid::new_v4();
    let changed = change::apply(&mut f.db, &f.who, id, &action, 10).unwrap();
    assert_eq!(
        saved(&f, main)
            .configuration
            .selection
            .model
            .unwrap()
            .as_str(),
        "explicit-model"
    );
    assert!(saved(&f, other).configuration.selection.model.is_none());
    let bot = read::bot(&f.db.connection, &f.who, f.bot.id).unwrap();
    assert_eq!(bot.revision, 1);
    assert!(bot.definition.configuration.selection.model.is_none());
    let mut defaults = bot.definition.configuration.clone();
    defaults.selection.model = Some("new-default".parse().unwrap());
    f.change(Change::Configure {
        bot: bot.id,
        revision: bot.revision,
        name: bot.definition.name,
        avatar: bot.definition.avatar,
        configuration: defaults,
        role: None,
    });
    assert!(saved(&f, other).configuration.selection.model.is_none());
    let later = side(&mut f).id;
    assert_eq!(
        saved(&f, later)
            .configuration
            .selection
            .model
            .unwrap()
            .as_str(),
        "new-default"
    );
    let replay = change::apply(&mut f.db, &f.who, id, &action, 15).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.answer, changed.answer);

    // The actual worker configuration path reads that conversation's choice.
    let worker = crate::authority::Worker {
        bot: bot.id,
        incarnation: Uuid::new_v4(),
    };
    f.db.connection.execute("INSERT INTO workers(bot,incarnation,started_at_ms,last_heard_at_ms) VALUES (?1,?2,1,1)", params![worker.bot.to_string(),worker.incarnation.to_string()]).unwrap();
    assert_eq!(
        f.db.configuration(worker, main)
            .unwrap()
            .selection
            .model
            .unwrap()
            .as_str(),
        "explicit-model"
    );
    assert!(
        f.db.configuration(worker, other)
            .unwrap()
            .selection
            .model
            .is_none()
    );
}

#[test]
fn selection_conflicts_and_failed_commits_preserve_the_prior_choice_and_receipt() {
    let mut f = Fixture::new();
    let main = f.bot.main_thread;
    let mut selection = saved(&f, main).configuration.selection;
    selection.fast = Some(true);
    let mut action = Change::ConversationSettings {
        bot: f.bot.id,
        thread: main,
        revision: 2,
        provider: f.bot.definition.configuration.provider.clone(),
        selection,
    };
    let id = Uuid::new_v4();
    assert_eq!(
        change::apply(&mut f.db, &f.who, id, &action, 10).err(),
        Some(Error::RevisionConflict)
    );
    if let Change::ConversationSettings { revision, .. } = &mut action {
        *revision = 1;
    }
    f.db.connection.execute_batch("CREATE TRIGGER fail_selection BEFORE UPDATE ON conversation_settings BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert_eq!(
        change::apply(&mut f.db, &f.who, id, &action, 10).err(),
        Some(Error::StorageUnavailable)
    );
    assert_eq!(saved(&f, main).revision, 1);
    assert_eq!(saved(&f, main).configuration.selection.fast, None);
    f.db.connection
        .execute_batch("DROP TRIGGER fail_selection;")
        .unwrap();
    let answer = change::apply(&mut f.db, &f.who, id, &action, 11).unwrap();
    assert!(!answer.replayed);
    assert_eq!(saved(&f, main).revision, 2);
    if let Change::ConversationSettings { selection, .. } = &mut action {
        selection.fast = Some(false);
    }
    assert_eq!(
        change::apply(&mut f.db, &f.who, id, &action, 12).err(),
        Some(Error::Conflict)
    );
    if let Change::ConversationSettings {
        revision, provider, ..
    } = &mut action
    {
        *revision = 2;
        *provider = "codex".parse().unwrap();
    }
    assert_eq!(
        change::apply(&mut f.db, &f.who, Uuid::new_v4(), &action, 12).err(),
        Some(Error::InvalidRequest)
    );
    let other = Person {
        owner: "other".into(),
        sender: f.who.sender.clone(),
    };
    assert_eq!(
        change::apply(&mut f.db, &other, Uuid::new_v4(), &action, 12).err(),
        Some(Error::NotAllowed)
    );
}
