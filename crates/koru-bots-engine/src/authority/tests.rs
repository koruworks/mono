// SPDX-License-Identifier: MPL-2.0
use super::*;
use serde_json::json;

#[test]
fn input_resolution_is_exactly_delivery_scoped_and_revoked_by_stop() {
    let mut f = Fixture::new();
    let file = crate::attachments::tests::record(f.thread, 12);
    f.db.stage_attachment(&f.person, &file).unwrap();
    f.db.accept_message(
        &f.person,
        &messages::Envelope {
            id: Uuid::new_v4(),
            to: f.worker.bot,
            thread: Some(f.thread),
            body: "file".into(),
            in_reply_to: None,
            attachments: vec![json!(file.attachment.id.as_uuid())],
            references: vec![],
            deadline_ms: None,
            reply_to: messages::ReplyTo::Thread,
        },
        1,
    )
    .unwrap();
    let issued = f.mint();
    let delivery = f.claim(issued.session);
    let secret = issued.secret.unwrap();
    let grant =
        f.db.resolve_input(&secret, delivery.turn, delivery.id)
            .unwrap();
    assert!(grant.delivery == delivery);
    assert_eq!(grant.files.len(), 1);
    assert_eq!(grant.files[0].attachment.id, file.attachment.id);
    assert!(grant.files[0].attachment.message_id.is_some());
    assert_eq!(format!("{grant:?}"), "InputGrant(<redacted>)");
    assert_eq!(
        f.db.resolve_input(&secret, delivery.turn, Uuid::new_v4())
            .unwrap_err(),
        Error::NotAllowed
    );
    let mut stale = delivery.turn;
    stale.generation += 1;
    assert_eq!(
        f.db.resolve_input(&secret, stale, delivery.id).unwrap_err(),
        Error::StaleTurn
    );
    f.db.stop_turn(&f.person, Uuid::new_v4(), delivery.turn, 4)
        .unwrap();
    assert_eq!(
        f.db.resolve_input(&secret, delivery.turn, delivery.id)
            .unwrap_err(),
        Error::CredentialRevoked
    );
}

#[test]
fn private_descriptor_contains_no_arguments_and_resolution_checks_its_exact_digest() {
    let mut f = Fixture::new();
    let issued = f.mint();
    f.claim(issued.session);
    let secret = issued.secret.unwrap();
    let call = Call::parse(
        Uuid::new_v4(),
        "read_document".into(),
        br#"{"page":"private-marker"}"#,
    )
    .unwrap();
    let bytes = serde_json::to_vec(call.descriptor()).unwrap();
    assert!(
        !String::from_utf8(bytes.clone())
            .unwrap()
            .contains("private-marker")
    );
    let descriptor: Descriptor = serde_json::from_slice(&bytes).unwrap();
    let binding =
        f.db.bind_descriptor(f.worker, &secret, &descriptor, Uuid::new_v4(), 4)
            .unwrap();
    assert_eq!(
        f.db.resolve_descriptor(&secret, binding.id, &descriptor)
            .unwrap()
            .binding,
        binding
    );
    let changed = Call::parse(
        call.door_call(),
        call.tool().into(),
        br#"{"page":"different"}"#,
    )
    .unwrap();
    assert_eq!(
        f.db.resolve_descriptor(&secret, binding.id, changed.descriptor())
            .err(),
        Some(Error::BindingConflict)
    );
    assert!(!format!("{descriptor:?}").contains("read_document"));
}

struct Fixture {
    db: Database,
    worker: Worker,
    thread: Uuid,
    person: messages::Person,
}
impl Fixture {
    fn new() -> Self {
        let db = Database::in_memory().unwrap();
        let bot = Uuid::new_v4();
        let thread = Uuid::new_v4();
        let incarnation = Uuid::new_v4();
        db.connection.execute("INSERT INTO bots(id,owner,name,settings,access,knowledge_pages,visibility) VALUES (?1,'owner','fixture',x'7b7d',?2,x'5b5d','private')",params![bot.to_string(),encode(&json!({"pages":["one","two"]})).unwrap()]).unwrap();
        db.connection.execute("INSERT INTO threads(id,bot,kind,title,lifecycle) VALUES (?1,?2,'main','','active')",params![thread.to_string(),bot.to_string()]).unwrap();
        db.connection.execute("INSERT INTO participants(thread,participant,kind) VALUES (?1,'person','person'),(?1,?2,'bot')",params![thread.to_string(),bot.to_string()]).unwrap();
        db.connection.execute("INSERT INTO workers(bot,incarnation,started_at_ms,last_heard_at_ms) VALUES (?1,?2,0,0)",params![bot.to_string(),incarnation.to_string()]).unwrap();
        let mut f = Self {
            db,
            worker: Worker { bot, incarnation },
            thread,
            person: messages::Person {
                owner: "owner".into(),
                sender: "person".into(),
            },
        };
        f.send();
        f
    }
    fn send(&mut self) {
        self.db
            .accept_message(
                &self.person,
                &messages::Envelope {
                    id: Uuid::new_v4(),
                    to: self.worker.bot,
                    thread: Some(self.thread),
                    in_reply_to: None,
                    body: "synthetic work".into(),
                    attachments: vec![],
                    references: vec![],
                    deadline_ms: None,
                    reply_to: messages::ReplyTo::Thread,
                },
                1,
            )
            .unwrap();
    }
    fn request(&self) -> Mint {
        Mint {
            command: Uuid::new_v4(),
            thread: self.thread,
            predecessor: self.db.session_head(self.worker, self.thread).unwrap(),
        }
    }
    fn mint(&mut self) -> Issued {
        self.db
            .mint_session(self.worker, self.request(), 2)
            .unwrap()
    }
    fn claim(&mut self, session: Uuid) -> messages::Delivery {
        self.db
            .claim_session_messages(self.worker, session, messages::Resume::Rebuilt, 65536, 3)
            .unwrap()
            .unwrap()
    }
    fn call(&self) -> Call {
        Call::parse(Uuid::new_v4(), "page-read".into(), br#"{"page":"one"}"#).unwrap()
    }
}

#[test]
fn mint_is_atomic_digest_only_and_never_replays_a_secret() {
    let mut f = Fixture::new();
    let request = f.request();
    let issued = f.db.mint_session(f.worker, request, 2).unwrap();
    let secret = issued.secret.as_ref().unwrap();
    assert!(!format!("{issued:?}").contains(secret.expose()));
    let receipt: Vec<u8> =
        f.db.connection
            .query_row(
                "SELECT answer FROM receipts WHERE id=?1",
                [request.command.to_string()],
                |r| r.get(0),
            )
            .unwrap();
    assert!(
        !String::from_utf8(receipt)
            .unwrap()
            .contains(secret.expose())
    );
    let digest: Vec<u8> =
        f.db.connection
            .query_row("SELECT digest FROM credentials", [], |r| r.get(0))
            .unwrap();
    assert_eq!(digest, secret.digest());
    let repeated = f.db.mint_session(f.worker, request, 3).unwrap();
    assert_eq!(repeated.session, issued.session);
    assert!(repeated.secret.is_none() && repeated.replayed);
    let mut conflict = request;
    conflict.predecessor = Some(issued.session);
    assert_eq!(
        f.db.mint_session(f.worker, conflict, 3).err(),
        Some(Error::Conflict)
    );
    let next = f.mint();
    assert_ne!(next.session, issued.session);
    assert_eq!(
        f.db.bind_call(f.worker, secret, &f.call(), Uuid::new_v4(), 4)
            .err(),
        Some(Error::CredentialRevoked)
    );
    assert_eq!(
        f.db.mint_session(
            f.worker,
            Mint {
                command: Uuid::new_v4(),
                ..request
            },
            4
        )
        .err(),
        Some(Error::SessionConflict)
    );
    // Issuance cannot become a covert secret archive after receipt pruning.
    f.db.connection
        .execute(
            "UPDATE receipts SET answer=NULL WHERE id=?1",
            [request.command.to_string()],
        )
        .unwrap();
    assert_eq!(
        f.db.mint_session(f.worker, request, 5).err(),
        Some(Error::Expired)
    );
}

#[test]
fn failed_mint_receipt_rolls_back_replacement_and_revocation() {
    let mut f = Fixture::new();
    let issued = f.mint();
    let secret = issued.secret.unwrap();
    f.db.connection.execute_batch("CREATE TRIGGER fail_receipt BEFORE INSERT ON receipts BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert_eq!(
        f.db.mint_session(f.worker, f.request(), 3).err(),
        Some(Error::StorageUnavailable)
    );
    assert_eq!(
        f.db.session_head(f.worker, f.thread).unwrap(),
        Some(issued.session)
    );
    f.db.connection
        .execute_batch("DROP TRIGGER fail_receipt;")
        .unwrap();
    f.claim(issued.session);
    assert!(
        f.db.bind_call(f.worker, &secret, &f.call(), Uuid::new_v4(), 4)
            .is_ok()
    );
}

#[test]
fn a_question_hold_does_not_allow_a_second_provider_credential() {
    let mut f = Fixture::new();
    let issued = f.mint();
    let delivery = f.claim(issued.session);
    f.db.question_event(
        delivery.turn,
        &koru_session_protocol::AgentEvent::ApprovalRequired {
            turn_id: delivery.turn.id.to_string().parse().unwrap(),
            approval_id: koru_session_protocol::ApprovalId::mint(),
            summary: "synthetic permission".into(),
            choices: vec![],
        },
        4,
    )
    .unwrap();
    assert_eq!(
        f.db.mint_session(f.worker, f.request(), 5).err(),
        Some(Error::TurnRunning)
    );
    assert_eq!(
        f.db.session_head(f.worker, f.thread).unwrap(),
        Some(issued.session)
    );
}

#[test]
fn binding_requires_an_open_turn_and_never_moves_to_the_next_turn() {
    let mut f = Fixture::new();
    let first = f.mint();
    let secret = first.secret.unwrap();
    let call = f.call();
    assert_eq!(
        f.db.bind_call(f.worker, &secret, &call, Uuid::new_v4(), 4)
            .err(),
        Some(Error::NoTurn)
    );
    let delivery = f.claim(first.session);
    assert_eq!(
        f.db.mint_session(f.worker, f.request(), 4).err(),
        Some(Error::TurnRunning)
    );
    let binding =
        f.db.bind_call(f.worker, &secret, &call, Uuid::new_v4(), 4)
            .unwrap();
    assert_eq!(binding.turn, delivery.turn);
    assert_eq!(
        f.db.bind_call(f.worker, &secret, &call, Uuid::new_v4(), 4)
            .unwrap(),
        binding
    );
    let changed = Call::parse(call.door_call(), call.tool().into(), br#"{"page":"two"}"#).unwrap();
    assert_eq!(
        f.db.bind_call(f.worker, &secret, &changed, Uuid::new_v4(), 4)
            .err(),
        Some(Error::BindingConflict)
    );
    f.db.interrupt_turn(delivery.turn).unwrap();
    assert_eq!(
        f.db.claim_session_messages(f.worker, first.session, messages::Resume::Rebuilt, 65536, 5)
            .err(),
        Some(Error::CredentialRevoked)
    );
    let second = f.mint();
    let second_secret = second.secret.unwrap();
    f.claim(second.session);
    assert_eq!(
        f.db.bind_call(f.worker, &secret, &call, Uuid::new_v4(), 4)
            .err(),
        Some(Error::CredentialRevoked)
    );
    assert_eq!(
        f.db.resolve_call(&second_secret, binding.id, &call).err(),
        Some(Error::BindingConflict)
    );
    let original: String =
        f.db.connection
            .query_row(
                "SELECT turn FROM tool_calls WHERE binding=?1",
                [binding.id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(original, delivery.turn.id.to_string());
}

#[test]
fn every_resolution_reads_current_grant_while_pause_allows_running_work() {
    let mut f = Fixture::new();
    let issued = f.mint();
    let secret = issued.secret.unwrap();
    f.claim(issued.session);
    let call = f.call();
    let binding =
        f.db.bind_call(f.worker, &secret, &call, Uuid::new_v4(), 4)
            .unwrap();
    f.db.connection
        .execute(
            "UPDATE bots SET access=?1,paused=1",
            [encode(&json!({"pages":["one"]})).unwrap()],
        )
        .unwrap();
    f.db.connection
        .execute("INSERT INTO team(owner,paused) VALUES ('owner',1)", [])
        .unwrap();
    let resolved = f.db.resolve_call(&secret, binding.id, &call).unwrap();
    assert_eq!(resolved.captured_grant, json!({"pages":["one","two"]}));
    assert_eq!(resolved.current_grant, json!({"pages":["one"]}));
    assert_eq!(resolved.scope, "owner");
    assert_eq!(resolved.bot, f.worker.bot);
    assert_eq!(resolved.thread, f.thread);
    f.db.connection
        .execute("UPDATE bots SET lifecycle='archived'", [])
        .unwrap();
    assert_eq!(
        f.db.resolve_call(&secret, binding.id, &call).err(),
        Some(Error::NotAllowed)
    );
}

#[test]
fn stop_and_recovery_revoke_before_any_late_call() {
    for mode in 0..3 {
        let mut f = Fixture::new();
        let issued = f.mint();
        let secret = issued.secret.unwrap();
        let delivery = f.claim(issued.session);
        let call = f.call();
        let binding =
            f.db.bind_call(f.worker, &secret, &call, Uuid::new_v4(), 4)
                .unwrap();
        match mode {
            0 => {
                f.db.stop_turn(&f.person, Uuid::new_v4(), delivery.turn, 4)
                    .unwrap();
            }
            1 => {
                f.db.interrupt_worker(f.worker.incarnation).unwrap();
            }
            _ => {
                f.db.recover_interrupted_turns().unwrap();
            }
        }
        assert_eq!(
            f.db.resolve_call(&secret, binding.id, &call).err(),
            Some(Error::CredentialRevoked)
        );
    }
    let mut f = Fixture::new();
    let issued = f.mint();
    let secret = issued.secret.unwrap();
    let delivery = f.claim(issued.session);
    f.db.connection.execute_batch("CREATE TRIGGER fail_revoke BEFORE UPDATE ON credentials BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert_eq!(
        f.db.stop_turn(&f.person, Uuid::new_v4(), delivery.turn, 4)
            .err(),
        Some(Error::StorageUnavailable)
    );
    assert!(
        f.db.bind_call(f.worker, &secret, &f.call(), Uuid::new_v4(), 4)
            .is_ok()
    );
    let state: String =
        f.db.connection
            .query_row("SELECT state FROM obligations", [], |r| r.get(0))
            .unwrap();
    assert_ne!(state, "cancelled");
}

#[test]
fn replaced_worker_cannot_bind_resolve_or_replay_issuance() {
    let mut f = Fixture::new();
    let request = f.request();
    let issued = f.db.mint_session(f.worker, request, 2).unwrap();
    let secret = issued.secret.unwrap();
    f.claim(issued.session);
    let call = f.call();
    let binding =
        f.db.bind_call(f.worker, &secret, &call, Uuid::new_v4(), 4)
            .unwrap();
    f.db.connection
        .execute(
            "UPDATE workers SET incarnation=?1",
            [Uuid::new_v4().to_string()],
        )
        .unwrap();
    assert_eq!(
        f.db.bind_call(f.worker, &secret, &call, Uuid::new_v4(), 4)
            .err(),
        Some(Error::WorkerReplaced)
    );
    assert_eq!(
        f.db.resolve_call(&secret, binding.id, &call).err(),
        Some(Error::WorkerReplaced)
    );
    assert_eq!(
        f.db.mint_session(f.worker, request, 5).err(),
        Some(Error::WorkerReplaced)
    );
}

#[test]
fn sibling_worker_and_changed_generation_cannot_use_a_binding() {
    let mut f = Fixture::new();
    let issued = f.mint();
    let secret = issued.secret.unwrap();
    let delivery = f.claim(issued.session);
    let call = f.call();
    let wrong = Worker {
        bot: Uuid::new_v4(),
        ..f.worker
    };
    assert_eq!(
        f.db.bind_call(wrong, &secret, &call, Uuid::new_v4(), 4)
            .err(),
        Some(Error::Unauthenticated)
    );
    let binding =
        f.db.bind_call(f.worker, &secret, &call, Uuid::new_v4(), 4)
            .unwrap();
    f.db.connection
        .execute(
            "UPDATE turns SET generation=generation+1 WHERE id=?1",
            [delivery.turn.id.to_string()],
        )
        .unwrap();
    assert_eq!(
        f.db.resolve_call(&secret, binding.id, &call).err(),
        Some(Error::BindingConflict)
    );
}

#[test]
fn descriptors_use_the_forwarded_value_and_reject_duplicate_keys_at_any_depth() {
    let door = Uuid::new_v4();
    let a = Call::parse(door, "read".into(), br#"{"b":[{"z":1,"a":2}],"a":null}"#).unwrap();
    let b = Call::parse(door, "read".into(), br#"{"a":null,"b":[{"a":2,"z":1}]}"#).unwrap();
    assert_eq!(a.argument_digest(), b.argument_digest());
    assert_eq!(a.arguments(), b.arguments());
    let c = Call::parse(door, "write".into(), br#"{"a":null,"b":[{"a":2,"z":1}]}"#).unwrap();
    assert_ne!(a.argument_digest(), c.argument_digest());
    for raw in [
        br#"{"a":1,"a":2}"#.as_slice(),
        br#"{"x":[{"a":1,"\u0061":2}]}"#,
        br#"[]"#,
        br#"{}{}"#,
    ] {
        assert!(Call::parse(door, "read".into(), raw).is_err());
    }
    assert!(serde_json::from_str::<Secret>("\"wrong-length\"").is_err());
}

#[test]
fn catalog_uses_the_start_grant_but_execution_requires_a_current_turn() {
    let mut f = Fixture::new();
    let issued = f.mint();
    let secret = issued.secret.unwrap();
    f.db.connection
        .execute(
            "UPDATE bots SET access=?1",
            [encode(&json!({"pages":["one"]})).unwrap()],
        )
        .unwrap();
    assert_eq!(
        f.db.session_catalog_grant(f.worker, &secret).unwrap(),
        json!({"pages":["one","two"]})
    );
    assert_eq!(
        f.db.bind_call(f.worker, &secret, &f.call(), Uuid::new_v4(), 4)
            .err(),
        Some(Error::NoTurn)
    );
    f.claim(issued.session);
    let call = f.call();
    let binding =
        f.db.bind_call(f.worker, &secret, &call, Uuid::new_v4(), 4)
            .unwrap();
    let resolution = f.db.resolve_call(&secret, binding.id, &call).unwrap();
    assert_eq!(resolution.current_grant, json!({"pages":["one"]}));
    f.db.interrupt_worker(f.worker.incarnation).unwrap();
    assert_eq!(
        f.db.session_catalog_grant(f.worker, &secret).err(),
        Some(Error::CredentialRevoked)
    );
}

#[test]
fn binding_commands_have_durable_conflict_and_expiry_semantics() {
    let mut f = Fixture::new();
    let issued = f.mint();
    let secret = issued.secret.unwrap();
    f.claim(issued.session);
    let call = f.call();
    let command = Uuid::new_v4();
    let bound =
        f.db.bind_call(f.worker, &secret, &call, command, 4)
            .unwrap();
    assert_eq!(
        f.db.bind_call(f.worker, &secret, &call, command, 5)
            .unwrap(),
        bound
    );
    assert_eq!(
        f.db.bind_call(f.worker, &secret, &f.call(), command, 5)
            .err(),
        Some(Error::Conflict)
    );
    f.db.connection
        .execute(
            "UPDATE receipts SET answer=NULL WHERE id=?1",
            [command.to_string()],
        )
        .unwrap();
    assert_eq!(
        f.db.bind_call(f.worker, &secret, &call, command, 6).err(),
        Some(Error::Expired)
    );
    f.db.connection
        .execute(
            "UPDATE workers SET incarnation=?1",
            [Uuid::new_v4().to_string()],
        )
        .unwrap();
    assert_eq!(
        f.db.bind_call(f.worker, &secret, &call, command, 7).err(),
        Some(Error::WorkerReplaced)
    );
}
