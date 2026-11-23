// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::bots_engine::window::readiness::Refusal;
use koru_ai_wire::bot_threads::{Command as C, Response as R};

fn principal() -> crate::GatewayPrincipal {
    crate::GatewayPrincipal {
        label: "synthetic".into(),
        scopes: vec![koru_ai_api::AiScope::ManageBots.as_str().into()],
    }
}
fn request(f: &Fixture, command: C) -> R {
    f.gateway
        .thread_command(&principal(), command, |_| Ok(()))
        .ok()
        .unwrap()
}

#[test]
fn native_rename_is_revision_checked_receipted_and_requires_bot_authority() {
    let f = Fixture::new();
    let bot = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    let read = || match request(
        &f,
        C::Read {
            thread: bot.main_chat,
        },
    ) {
        R::Read { snapshot } => snapshot,
        _ => panic!("thread snapshot"),
    };
    let before = read();
    let command = C::Rename {
        id: Uuid::new_v4(),
        thread: bot.main_chat,
        revision: before.revision,
        title: "News discussion".into(),
    };
    let denied = crate::GatewayPrincipal {
        label: "synthetic".into(),
        scopes: vec![],
    };
    assert!(matches!(
        f.gateway
            .thread_command(&denied, command.clone(), |_| Ok(())),
        Err(Refusal::Engine(Error::NotAllowed))
    ));
    assert!(matches!(
        request(&f, command.clone()),
        R::Changed {
            replayed: false,
            ..
        }
    ));
    assert!(matches!(
        request(&f, command),
        R::Changed { replayed: true, .. }
    ));
    let after = read();
    assert_eq!(after.conversation.chat.title, "News discussion");
    assert_eq!(after.revision, before.revision + 1);
    assert_eq!(after.settings_revision, before.settings_revision);
    let stale = C::Rename {
        id: Uuid::new_v4(),
        thread: bot.main_chat,
        revision: before.revision,
        title: "Stale title".into(),
    };
    assert!(matches!(
        f.gateway.thread_command(&principal(), stale, |_| Ok(())),
        Err(Refusal::Engine(Error::RevisionConflict))
    ));
    assert_eq!(read().conversation.chat.title, "News discussion");
}
#[test]
fn typed_chat_uses_actual_message_identity_receipts_and_history() {
    let f = Fixture::new();
    let bot = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    let command = C::Send {
        id: Uuid::new_v4(),
        bot: bot.id,
        thread: bot.main_chat,
        body: "Synthetic input".into(),
        in_reply_to: None,
        attachments: vec![],
        references: vec![],
    };
    let R::Accepted {
        message,
        sequence,
        replayed,
        ..
    } = request(&f, command.clone())
    else {
        panic!()
    };
    assert!(!replayed);
    let R::Accepted {
        message: again,
        replayed,
        ..
    } = request(&f, command)
    else {
        panic!()
    };
    assert!(replayed);
    assert_eq!(message, again);
    let R::Read { snapshot } = request(
        &f,
        C::Read {
            thread: bot.main_chat,
        },
    ) else {
        panic!()
    };
    assert_eq!(snapshot.head_sequence, sequence);
    assert_eq!(snapshot.waiting_messages, 1);
    assert!(snapshot.turns.is_empty()); // Acceptance does not fabricate a turn.
    assert_eq!(snapshot.conversation.chat.id, bot.main_chat);
    let R::History {
        messages,
        before,
        after,
    } = request(
        &f,
        C::History {
            thread: bot.main_chat,
            before: None,
        },
    )
    else {
        panic!()
    };
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].id, message);
    assert_eq!(messages[0].content.body, "Synthetic input");
    assert_eq!(messages[0].created_at_ms, 2);
    assert_eq!(before, None);
    assert_eq!(after, sequence);
    let R::History {
        messages, after, ..
    } = request(
        &f,
        C::Since {
            thread: bot.main_chat,
            after,
        },
    )
    else {
        panic!()
    };
    assert!(messages.is_empty());
    assert_eq!(after, sequence);
    let R::Observed {
        boot,
        cursor,
        gap,
        entries,
    } = request(
        &f,
        C::Observe {
            thread: bot.main_chat,
            boot: None,
            after: 0,
        },
    )
    else {
        panic!()
    };
    assert!(gap);
    assert!(entries.is_empty());
    let R::Observed { gap, .. } = request(
        &f,
        C::Observe {
            thread: bot.main_chat,
            boot: Some(boot),
            after: cursor,
        },
    ) else {
        panic!()
    };
    assert!(!gap);
    let R::Attention { items, next } = request(
        &f,
        C::Attention {
            thread: bot.main_chat,
            after: None,
        },
    ) else {
        panic!()
    };
    assert!(items.is_empty());
    assert!(next.is_none());
    assert!(matches!(
        f.gateway.thread_command(
            &principal(),
            C::Continue {
                id: Uuid::new_v4(),
                thread: bot.main_chat,
                operations: vec![]
            },
            |_| Ok(())
        ),
        Err(Refusal::Engine(Error::InvalidRequest))
    ));
    f.store.moved.store(true, Ordering::Relaxed);
    assert!(matches!(
        f.gateway.thread_command(
            &principal(),
            C::History {
                thread: bot.main_chat,
                before: None
            },
            |_| Ok(())
        ),
        Err(Refusal::Engine(Error::NotAllowed))
    ));
}

#[test]
fn typed_settings_keep_exact_receipts_and_validate_before_mutation() {
    let f = Fixture::new();
    let bot = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    let mut model = bot.config.model.clone();
    model.requested_model = "specific".into();
    let command = C::Settings {
        id: Uuid::new_v4(),
        bot: bot.id,
        thread: bot.main_chat,
        revision: 1,
        model,
        mode: None,
        fast: Some(true),
    };
    assert!(matches!(
        f.gateway
            .thread_command(&principal(), command.clone(), |c| {
                crate::bots_engine::window::readiness::validate(None, c)
            }),
        Err(Refusal::Runtime(_))
    ));
    let R::Read { snapshot } = request(
        &f,
        C::Read {
            thread: bot.main_chat,
        },
    ) else {
        panic!()
    };
    assert_eq!(snapshot.settings_revision, 1);
    let R::Settings {
        revision,
        model,
        replayed,
        ..
    } = request(&f, command.clone())
    else {
        panic!()
    };
    assert_eq!(revision, 2);
    assert_eq!(model.requested_model, "specific");
    assert!(!replayed);
    let R::Settings {
        revision, replayed, ..
    } = f
        .gateway
        .thread_command(&principal(), command, |_| {
            panic!("replayed settings rediscovered runtime")
        })
        .ok()
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(revision, 2);
    assert!(replayed);
    let denied = crate::GatewayPrincipal {
        label: "synthetic".into(),
        scopes: vec![],
    };
    assert!(matches!(
        f.gateway.thread_command(
            &denied,
            C::Read {
                thread: bot.main_chat
            },
            |_| Ok(())
        ),
        Err(Refusal::Engine(Error::NotAllowed))
    ));
}

/// The engine's history read answers with one synthetic message at the
/// asked position: a published Bot reply after sequence 6, a person's
/// words anywhere else. Every other request reaches the real engine.
fn with_history(f: &mut Fixture, reply: Uuid, words: Uuid) {
    let inner = f.gateway.call.clone();
    f.gateway.call = Arc::new(move |request: person::Request| {
        if let Command::Read {
            query: Query::History { after, .. },
        } = &request.command
        {
            let (id, sender, body) = if *after == 6 {
                (reply, "bot:synthetic", "# Plan\n\nShip it on Friday.")
            } else {
                (words, "person:synthetic", "My own words")
            };
            return Ok(person::Reply(serde_json::json!([{
                "id": id,
                "sequence": after + 1,
                "sender": sender,
                "kind": "words",
                "content": {
                    "body": body,
                    "attachments": [],
                    "references": [],
                    "in_reply_to": null,
                    "deadline_ms": null,
                    "reply_to": "thread",
                },
                "created_at_ms": 2,
            }])));
        }
        inner(request)
    });
}

#[test]
fn a_bot_reply_saves_once_per_request_into_the_bots_workspace() {
    let mut f = Fixture::new();
    let bot = creation(f.gateway.window_command(form(&f)).unwrap())
        .bot
        .unwrap();
    let (reply, words) = (Uuid::new_v4(), Uuid::new_v4());
    with_history(&mut f, reply, words);
    let writes = f.store.writes.load(Ordering::Relaxed);
    let save = C::SaveReply {
        id: Uuid::new_v4(),
        thread: bot.main_chat,
        message: reply,
        sequence: 7,
        title: "  Launch plan ".into(),
        document: Uuid::new_v4(),
    };
    let C::SaveReply { document, .. } = save.clone() else {
        unreachable!()
    };
    let R::Saved {
        document: saved,
        workspace,
        title,
        ..
    } = request(&f, save.clone())
    else {
        panic!("saved")
    };
    assert_eq!(saved, document);
    assert_eq!(workspace, f.store.home.as_uuid(), "the Bot's own workspace");
    assert_eq!(title, "Launch plan");
    assert_eq!(f.store.writes.load(Ordering::Relaxed), writes + 1);

    // A retry after a lost answer finds the page it made.
    assert!(matches!(request(&f, save.clone()), R::Saved { .. }));
    assert_eq!(f.store.writes.load(Ordering::Relaxed), writes + 1);

    // Another request cannot claim that page.
    let C::SaveReply {
        thread,
        message,
        sequence,
        title,
        ..
    } = save
    else {
        unreachable!()
    };
    let rival = C::SaveReply {
        id: Uuid::new_v4(),
        thread,
        message,
        sequence,
        title,
        document,
    };
    assert!(matches!(
        f.gateway.thread_command(&principal(), rival, |_| Ok(())),
        Err(Refusal::Engine(Error::Conflict))
    ));

    // Only the engine's copy of a Bot reply is saved: a person's message,
    // or an id that is not at that position, is refused.
    for (message, sequence) in [(words, 3), (Uuid::new_v4(), 7)] {
        let refused = C::SaveReply {
            id: Uuid::new_v4(),
            thread: bot.main_chat,
            message,
            sequence,
            title: "Not a reply".into(),
            document: Uuid::new_v4(),
        };
        assert!(matches!(
            f.gateway.thread_command(&principal(), refused, |_| Ok(())),
            Err(Refusal::Engine(Error::InvalidRequest))
        ));
    }
    assert_eq!(f.store.writes.load(Ordering::Relaxed), writes + 1);
}
