// SPDX-License-Identifier: MPL-2.0
use super::*;

struct Fixture {
    db: Database,
    begin: Command,
    id: Uuid,
    bot: Uuid,
}
impl Fixture {
    fn new(authored: bool) -> Self {
        let workspace = koru_delegate_core::WorkspaceId::new();
        let id = Uuid::new_v4();
        let bot = Uuid::new_v4();
        let definition = Definition {
            name: "synthetic".into(),
            avatar: Some(3),
            configuration: crate::configuration::Configuration {
                provider: "claude-acp".parse().unwrap(),
                selection: Default::default(),
                working_folder: None,
            },
            access: koru_delegate_core::CapturedBotPolicy {
                workspace,
                destination: workspace,
                access: koru_delegate_core::BotAccess::Default,
                selected_pages: Default::default(),
                connections: Default::default(),
            },
            role: if authored { None } else { Some(Uuid::new_v4()) },
            knowledge: vec![],
            memory: None,
        };
        Self {
            db: Database::in_memory().unwrap(),
            id,
            bot,
            begin: Command::Begin {
                request: id,
                bot,
                definition,
                authored: authored.then(|| AuthoredRole {
                    title: "Synthetic Role".into(),
                    markdown: "Synthetic instructions".into(),
                }),
                seed: authored.then(|| vec![10, 20, 30]),
            },
        }
    }
    fn call(&mut self, command: Command) -> Result<Reply, Error> {
        handle(
            &mut self.db,
            Request {
                owner: "owner".into(),
                sender: "person:owner".into(),
                command,
            },
            1,
        )
    }
    fn begin(&mut self) -> Record {
        record(self.call(self.begin.clone()).unwrap())
    }
    fn read(&mut self) -> Record {
        record(self.call(Command::Read { request: self.id }).unwrap())
    }
    fn advance(&mut self, r: &Record, step: Step) -> Result<Record, Error> {
        self.call(Command::Advance {
            request: self.id,
            revision: r.revision,
            step,
        })
        .map(record)
    }
    fn ready(&mut self) -> Record {
        let r = self.begin();
        let r = self.advance(&r, Step::StartFolder).unwrap();
        let r = self
            .advance(&r, Step::FolderReady { binding: folder() })
            .unwrap();
        if r.seed.is_some() {
            let r = self.advance(&r, Step::StartRole).unwrap();
            self.advance(&r, Step::RoleVerified).unwrap()
        } else {
            r
        }
    }
    fn count(&self, table: &str) -> i64 {
        assert!(matches!(
            table,
            "bots" | "threads" | "participants" | "creations"
        ));
        self.db
            .connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
}
fn record(reply: Reply) -> Record {
    match reply {
        Reply::Record(Some(r)) => r,
        _ => panic!("record expected"),
    }
}
fn folder() -> String {
    if cfg!(windows) {
        r"C:\synthetic\workspace".into()
    } else {
        "/synthetic/workspace".into()
    }
}

#[test]
fn lost_begin_reply_keeps_exact_seed_and_reserves_bot_before_effects() {
    let mut f = Fixture::new(true);
    let first = f.begin();
    assert_eq!(f.count("bots"), 0);
    assert_eq!(f.count("threads"), 0);
    // A fresh Loro export can differ even for identical authored text. The
    // retry must return the first durable export and its Store request ids.
    if let Command::Begin { seed, .. } = &mut f.begin {
        *seed = Some(vec![40, 50]);
    }
    let retry = f.begin();
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::to_value(&retry).unwrap()
    );
    let who = Person {
        owner: "owner".into(),
        sender: "person:owner".into(),
    };
    let action = person::Change::Create {
        bot: f.bot,
        definition: first.definition.clone(),
    };
    assert_eq!(
        person::handle(
            &mut f.db,
            &Default::default(),
            &mut Default::default(),
            person::Request {
                owner: who.owner,
                sender: who.sender,
                command: person::Command::Change {
                    id: Uuid::new_v4(),
                    action
                },
            },
            1
        )
        .err(),
        Some(Error::Conflict)
    );
    if let Command::Begin {
        authored: Some(role),
        ..
    } = &mut f.begin
    {
        role.markdown.push('!');
    }
    assert_eq!(f.call(f.begin.clone()).err(), Some(Error::Conflict));
    assert_eq!(f.count("creations"), 1);
}

#[test]
fn completion_commits_bot_main_thread_and_receipt_once() {
    for authored in [false, true] {
        let mut f = Fixture::new(authored);
        let r = f.ready();
        let completed = f.advance(&r, Step::Complete).unwrap();
        assert_eq!(completed.phase, Phase::Completed);
        assert!(
            completed
                .seed
                .as_ref()
                .is_none_or(|seed| seed.bytes.is_empty())
        );
        let bot = completed.result.as_ref().unwrap();
        assert_eq!(bot.id, f.bot);
        assert_eq!(bot.definition.avatar, Some(3));
        assert_eq!(bot.definition.role, completed.definition.role);
        assert_eq!(f.count("bots"), 1);
        assert_eq!(f.count("threads"), 1);
        assert_eq!(f.count("participants"), 2);
        assert_eq!(f.advance(&r, Step::Complete).err(), Some(Error::Conflict));
        assert_eq!(f.begin().result.unwrap().main_thread, bot.main_thread);
        let Reply::Pending(records) = f.call(Command::Pending { after: None }).unwrap() else {
            panic!()
        };
        assert!(records.is_empty());
    }
}

#[test]
fn completion_rollback_leaves_no_partial_bot_and_can_retry() {
    let mut f = Fixture::new(true);
    let ready = f.ready();
    f.db.connection.execute_batch("CREATE TRIGGER fail_creation BEFORE UPDATE ON creations BEGIN SELECT RAISE(ABORT, 'injected'); END;").unwrap();
    assert!(f.advance(&ready, Step::Complete).is_err());
    assert_eq!(f.count("bots"), 0);
    assert_eq!(f.count("threads"), 0);
    assert_eq!(f.count("participants"), 0);
    assert_eq!(f.read().phase, Phase::Ready);
    f.db.connection
        .execute_batch("DROP TRIGGER fail_creation")
        .unwrap();
    assert_eq!(
        f.advance(&ready, Step::Complete).unwrap().phase,
        Phase::Completed
    );
}

#[test]
fn stale_effects_cannot_advance_abandoned_or_parked_creation() {
    let mut f = Fixture::new(true);
    let intent = f.begin();
    assert_eq!(
        f.advance(&intent, Step::Complete).err(),
        Some(Error::NotAllowed)
    );
    let pending = f.advance(&intent, Step::StartFolder).unwrap();
    let failed = f
        .advance(
            &pending,
            Step::Failed {
                reason: Failure::FolderUnavailable,
            },
        )
        .unwrap();
    assert_eq!(
        f.advance(&failed, Step::FolderReady { binding: folder() })
            .err(),
        Some(Error::NotAllowed)
    );
    let resumed = f.advance(&failed, Step::Retry).unwrap();
    assert_eq!(resumed.phase, Phase::FolderPending);
    assert!(resumed.failure.is_none());
    let abandoned = f.advance(&resumed, Step::Abandon).unwrap();
    assert_eq!(
        f.advance(&resumed, Step::FolderReady { binding: folder() })
            .err(),
        Some(Error::Conflict)
    );
    assert_eq!(
        f.advance(&abandoned, Step::Retry).err(),
        Some(Error::NotAllowed)
    );
    assert_eq!(f.begin().phase, Phase::Abandoned);
    assert_eq!(f.count("bots"), 0);
    assert_eq!(f.count("creations"), 1);
    // Abandonment retains identities that may have reached the Store.
    assert!(abandoned.seed.as_ref().unwrap().bytes.is_empty());
    assert_eq!(
        abandoned.seed.unwrap().candidate,
        intent.seed.unwrap().candidate
    );
}

#[test]
fn reads_are_owner_scoped_and_pending_projection_omits_seed_bytes() {
    let mut f = Fixture::new(true);
    let r = f.begin();
    let reply = handle(
        &mut f.db,
        Request {
            owner: "another".into(),
            sender: "person:another".into(),
            command: Command::Read { request: f.id },
        },
        2,
    )
    .unwrap();
    assert!(matches!(reply, Reply::Record(None)));
    assert_eq!(
        handle(
            &mut f.db,
            Request {
                owner: "another".into(),
                sender: "person:another".into(),
                command: Command::Advance {
                    request: f.id,
                    revision: r.revision,
                    step: Step::Abandon
                }
            },
            2
        )
        .err(),
        Some(Error::NotAllowed)
    );
    let Reply::Pending(list) = f.call(Command::Pending { after: None }).unwrap() else {
        panic!()
    };
    assert_eq!(list.len(), 1);
    assert!(list[0].seed.as_ref().unwrap().bytes.is_empty());
    assert_eq!(f.read().seed.unwrap().bytes, vec![10, 20, 30]);
    let debug = format!("{r:?}");
    assert!(!debug.contains("synthetic"));
}

#[test]
fn role_cannot_be_verified_before_folder_and_seeds_require_authored_intent() {
    let mut f = Fixture::new(true);
    let intent = f.begin();
    assert_eq!(
        f.advance(&intent, Step::RoleVerified).err(),
        Some(Error::NotAllowed)
    );
    let pending = f.advance(&intent, Step::StartFolder).unwrap();
    assert_eq!(
        f.advance(
            &pending,
            Step::FolderReady {
                binding: "relative".into()
            }
        )
        .err(),
        Some(Error::InvalidRequest)
    );
    let mut f = Fixture::new(false);
    if let Command::Begin { seed, .. } = &mut f.begin {
        *seed = Some(vec![1]);
    }
    assert_eq!(f.call(f.begin.clone()).err(), Some(Error::InvalidRequest));
    assert_eq!(f.count("creations"), 0);
}

#[test]
fn pending_limit_is_released_only_by_terminal_creation_and_pages_are_bounded() {
    let mut f = Fixture::new(false);
    let first = f.begin();
    for _ in 1..64 {
        let mut next = f.begin.clone();
        if let Command::Begin { request, bot, .. } = &mut next {
            *request = Uuid::new_v4();
            *bot = Uuid::new_v4();
        }
        f.call(next).unwrap();
    }
    let mut next = f.begin.clone();
    if let Command::Begin { request, bot, .. } = &mut next {
        *request = Uuid::new_v4();
        *bot = Uuid::new_v4();
    }
    assert_eq!(f.call(next.clone()).err(), Some(Error::EngineUnavailable));
    let Reply::Pending(page) = f.call(Command::Pending { after: None }).unwrap() else {
        panic!()
    };
    assert_eq!(page.len(), 32);
    let Reply::Pending(second) = f
        .call(Command::Pending {
            after: Some(page.last().unwrap().request),
        })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(second.len(), 32);
    assert!(second[0].request.to_string() > page.last().unwrap().request.to_string());
    let failed = f
        .advance(
            &first,
            Step::Failed {
                reason: Failure::FolderUnavailable,
            },
        )
        .unwrap();
    assert_eq!(f.call(next.clone()).err(), Some(Error::EngineUnavailable));
    f.advance(&failed, Step::Abandon).unwrap();
    f.call(next).unwrap();
    assert_eq!(f.count("creations"), 65);
}
