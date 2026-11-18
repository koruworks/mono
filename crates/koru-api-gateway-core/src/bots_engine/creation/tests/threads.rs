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
