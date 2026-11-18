// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::capacity::{Admission, Decision};
use rusqlite::params;

struct Fixture {
    db: Database,
    who: Person,
    bot: Bot,
}
impl Fixture {
    fn new() -> Self {
        let mut db = Database::in_memory().unwrap();
        let who = Person {
            owner: Uuid::new_v4().to_string(),
            sender: "person".into(),
        };
        let workspace = koru_delegate_core::WorkspaceId::new();
        let definition = Definition {
            name: "Scout".into(),
            avatar: None,
            configuration: Configuration {
                provider: "claude-acp".parse().unwrap(),
                selection: Default::default(),
                working_folder: None,
            },
            access: CapturedBotPolicy {
                workspace,
                destination: workspace,
                access: koru_delegate_core::BotAccess::Default,
                selected_pages: Default::default(),
                connections: Default::default(),
            },
            role: None,
            knowledge: vec![],
            memory: None,
        };
        let action = Change::Create {
            bot: Uuid::new_v4(),
            definition,
        };
        let bot = serde_json::from_value(
            change::apply(&mut db, &who, Uuid::new_v4(), &action, 1)
                .unwrap()
                .answer,
        )
        .unwrap();
        Self { db, who, bot }
    }
    fn change(&mut self, action: Change) -> Value {
        change::apply(&mut self.db, &self.who, Uuid::new_v4(), &action, 5)
            .unwrap()
            .answer
    }
    fn send(&mut self, thread: Uuid) -> messages::Accepted {
        self.db
            .accept_message(
                &self.who,
                &messages::Envelope {
                    id: Uuid::new_v4(),
                    to: self.bot.id,
                    thread: Some(thread),
                    in_reply_to: None,
                    body: "work".into(),
                    attachments: vec![],
                    references: vec![],
                    deadline_ms: None,
                    reply_to: messages::ReplyTo::Thread,
                },
                3,
            )
            .unwrap()
            .answer
    }
}

#[test]
fn create_and_change_retries_are_immutable_and_cross_owner_requests_fail() {
    let mut f = Fixture::new();
    let command = Uuid::new_v4();
    let action = Change::Pause {
        bot: f.bot.id,
        revision: 1,
        paused: true,
    };
    let first = change::apply(&mut f.db, &f.who, command, &action, 2).unwrap();
    let second = change::apply(&mut f.db, &f.who, command, &action, 3).unwrap();
    assert!(second.replayed);
    assert_eq!(first.answer, second.answer);
    assert_eq!(first.answer["revision"], 2);
    assert_eq!(
        change::apply(&mut f.db, &f.who, Uuid::new_v4(), &action, 3).err(),
        Some(Error::RevisionConflict)
    );
    let other = Person {
        owner: "another-owner".into(),
        sender: f.who.sender.clone(),
    };
    assert_eq!(
        read::bot(&f.db.connection, &other, f.bot.id).err(),
        Some(Error::NotAllowed)
    );
    assert_eq!(
        change::apply(&mut f.db, &other, command, &action, 3).err(),
        Some(Error::NotAllowed)
    );
    let mut changed = action.clone();
    if let Change::Pause { paused, .. } = &mut changed {
        *paused = false;
    }
    assert_eq!(
        change::apply(&mut f.db, &f.who, command, &changed, 3).err(),
        Some(Error::Conflict)
    );
}

#[test]
fn pause_accepts_work_and_team_resume_obeys_each_bots_own_pause() {
    let mut f = Fixture::new();
    let thread = f.bot.main_thread;
    f.change(Change::Pause {
        bot: f.bot.id,
        revision: 1,
        paused: true,
    });
    f.send(thread);
    let mut admission = Admission::new(1);
    assert!(admission.next(&f.db, 4).unwrap().is_none());
    f.change(Change::Team {
        revision: 0,
        paused: true,
    });
    f.change(Change::Team {
        revision: 1,
        paused: false,
    });
    assert!(admission.next(&f.db, 6).unwrap().is_none());
    f.change(Change::Pause {
        bot: f.bot.id,
        revision: 2,
        paused: false,
    });
    assert!(matches!(
        admission.next(&f.db, 6).unwrap(),
        Some(Decision::Start { .. })
    ));
}

#[test]
fn trash_stops_reserved_work_restore_preserves_content_without_restart() {
    let mut f = Fixture::new();
    let thread = f.bot.main_thread;
    f.send(thread);
    let mut admission = Admission::new(1);
    let Some(Decision::Start { reservation, .. }) = admission.next(&f.db, 4).unwrap() else {
        panic!("start")
    };
    let worker = Uuid::new_v4();
    admission.bind_worker(f.bot.id, worker);
    f.change(Change::BotLifecycle {
        bot: f.bot.id,
        revision: 1,
        lifecycle: Lifecycle::Trashed,
    });
    admission.refresh_stops(&f.db).unwrap();
    assert_eq!(admission.retiring(f.bot.id, worker), vec![reservation]);
    assert_eq!(admission.occupied(), 1);
    f.change(Change::BotLifecycle {
        bot: f.bot.id,
        revision: 2,
        lifecycle: Lifecycle::Active,
    });
    admission.retired(reservation, worker).unwrap();
    assert!(admission.next(&f.db, 9).unwrap().is_none());
    assert_eq!(f.db.read_messages(&f.who, thread, 0).unwrap().len(), 1);
    assert_eq!(
        f.db.connection
            .query_row(
                "SELECT count(*) FROM obligations WHERE state='cancelled'",
                [],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        1
    );
}

#[test]
fn conversation_card_commits_once_and_thread_trash_does_not_stop_a_sibling() {
    let mut f = Fixture::new();
    let command = Uuid::new_v4();
    let action = Change::Conversation {
        bot: f.bot.id,
        title: "Side conversation".into(),
    };
    let response = change::apply(&mut f.db, &f.who, command, &action, 3).unwrap();
    let thread: Thread = serde_json::from_value(response.answer.clone()).unwrap();
    assert!(
        change::apply(&mut f.db, &f.who, command, &action, 4)
            .unwrap()
            .replayed
    );
    assert_eq!(
        f.db.read_messages(&f.who, f.bot.main_thread, 0)
            .unwrap()
            .len(),
        1
    );
    f.send(thread.id);
    f.send(f.bot.main_thread);
    f.change(Change::ThreadLifecycle {
        thread: thread.id,
        revision: 1,
        lifecycle: Lifecycle::Trashed,
    });
    assert_eq!(
        f.db.connection
            .query_row(
                "SELECT count(*) FROM inbox WHERE thread=?1",
                [f.bot.main_thread.to_string()],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        f.db.connection
            .query_row(
                "SELECT count(*) FROM inbox WHERE thread=?1",
                [thread.id.to_string()],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        0
    );
    f.change(Change::ThreadLifecycle {
        thread: thread.id,
        revision: 2,
        lifecycle: Lifecycle::Active,
    });
    assert_eq!(f.db.read_messages(&f.who, thread.id, 0).unwrap().len(), 1);
}

#[test]
fn failed_configuration_transaction_rolls_back_and_setup_retry_is_exact() {
    let mut f = Fixture::new();
    f.db.connection.execute_batch("CREATE TRIGGER fail_configuration BEFORE UPDATE OF settings ON bots BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    let action = Change::Configure {
        bot: f.bot.id,
        revision: 1,
        name: "new".into(),
        avatar: None,
        role: Some(Uuid::new_v4()),
        configuration: f.bot.definition.configuration.clone(),
    };
    assert_eq!(
        change::apply(&mut f.db, &f.who, Uuid::new_v4(), &action, 3).err(),
        Some(Error::StorageUnavailable)
    );
    let current = read::bot(&f.db.connection, &f.who, f.bot.id).unwrap();
    assert_eq!(current.definition.name, "Scout");
    assert_eq!(current.definition.role, f.bot.definition.role);
    assert_eq!(current.revision, 1);
    let turn = Uuid::new_v4();
    f.db.connection.execute("INSERT INTO turns(id,thread,bot,grant,state,generation,details,created_at_ms) VALUES (?1,?2,?3,x'7b7d','needs_you',1,?4,1)",params![turn.to_string(),f.bot.main_thread.to_string(),f.bot.id.to_string(),br#"{"setup_failure":"configuration"}"#.as_slice()]).unwrap();
    f.change(Change::RetrySetup {
        thread: f.bot.main_thread,
        turn,
    });
    assert_eq!(
        change::apply(
            &mut f.db,
            &f.who,
            Uuid::new_v4(),
            &Change::RetrySetup {
                thread: f.bot.main_thread,
                turn
            },
            4
        )
        .err(),
        Some(Error::StaleTurn)
    );
}

#[test]
fn selected_folder_freezes_on_accepted_input_but_model_and_face_stay_editable() {
    let mut f = Fixture::new();
    let mut config = f.bot.definition.configuration.clone();
    let path = if cfg!(windows) {
        r"C:\synthetic\project"
    } else {
        "/synthetic/project"
    };
    config.working_folder = Some(path.into());
    let action = Change::Configure {
        bot: f.bot.id,
        revision: 1,
        name: "Scout".into(),
        avatar: Some(7),
        role: None,
        configuration: config.clone(),
    };
    let value = f.change(action);
    assert_eq!(value["definition"]["avatar"], 7);
    assert_eq!(value["definition"]["configuration"]["working_folder"], path);
    f.send(f.bot.main_thread);
    config.working_folder = None;
    let action = Change::Configure {
        bot: f.bot.id,
        revision: 2,
        name: "Scout".into(),
        avatar: Some(8),
        role: None,
        configuration: config.clone(),
    };
    assert_eq!(
        change::apply(&mut f.db, &f.who, Uuid::new_v4(), &action, 8).err(),
        Some(Error::FolderFrozen)
    );
    config.working_folder = Some(path.into());
    let action = Change::Configure {
        bot: f.bot.id,
        revision: 2,
        name: "Scout".into(),
        avatar: Some(8),
        role: None,
        configuration: config,
    };
    assert_eq!(f.change(action)["definition"]["avatar"], 8);
}

#[test]
fn window_policy_revision_changes_only_with_access_and_freeze_time_is_durable() {
    let mut f = Fixture::new();
    let changed = f.change(Change::Configure {
        bot: f.bot.id,
        revision: 1,
        name: "renamed".into(),
        avatar: Some(5),
        role: None,
        configuration: f.bot.definition.configuration.clone(),
    });
    assert_eq!(changed["revision"], 2);
    assert_eq!(changed["policy_revision"], 1);
    let mut access = f.bot.definition.access.clone();
    access
        .selected_pages
        .insert(koru_delegate_core::DocumentId::new());
    let changed = f.change(Change::Access {
        bot: f.bot.id,
        revision: 2,
        access,
    });
    assert_eq!(changed["revision"], 3);
    assert_eq!(changed["policy_revision"], 2);
    assert!(
        read::bot(&f.db.connection, &f.who, f.bot.id)
            .unwrap()
            .folder_frozen_at_ms
            .is_none()
    );
    f.send(f.bot.main_thread);
    assert_eq!(
        read::bot(&f.db.connection, &f.who, f.bot.id)
            .unwrap()
            .folder_frozen_at_ms,
        Some(3)
    );
}

#[test]
fn list_is_owner_scoped_and_paged_without_dropping_entries() {
    let mut f = Fixture::new();
    for _ in 0..65 {
        f.change(Change::Create {
            bot: Uuid::new_v4(),
            definition: f.bot.definition.clone(),
        });
    }
    let workspace = f.bot.definition.access.workspace.as_uuid();
    let first = read::query(
        &f.db,
        &f.who,
        Query::Bots {
            workspace,
            after: None,
            lifecycle: Lifecycle::Active,
        },
    )
    .unwrap();
    assert_eq!(first["items"].as_array().unwrap().len(), 64);
    let second = read::query(
        &f.db,
        &f.who,
        Query::Bots {
            workspace,
            after: Some(first["next"].as_str().unwrap().parse().unwrap()),
            lifecycle: Lifecycle::Active,
        },
    )
    .unwrap();
    assert_eq!(second["items"].as_array().unwrap().len(), 2);
    assert!(second["next"].is_null());
    let other = Person {
        owner: "stranger".into(),
        sender: f.who.sender.clone(),
    };
    let empty = read::query(
        &f.db,
        &other,
        Query::Bots {
            workspace,
            after: None,
            lifecycle: Lifecycle::Active,
        },
    )
    .unwrap();
    assert!(empty["items"].as_array().unwrap().is_empty());
}
