// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::managed_surface::tests::surface;
use koru_ai_wire::bot_threads as wire;

fn message(sequence: u64, sender: &str) -> wire::Message {
    wire::Message {
        id: Uuid::new_v4(),
        sequence,
        sender: sender.into(),
        kind: "words".into(),
        content: wire::Content {
            body: "# Plan\n\nShip it on Friday.".into(),
            attachments: vec![],
            references: vec![],
            in_reply_to: None,
            deadline_ms: None,
            reply_to: "thread".into(),
        },
        created_at_ms: 2,
    }
}

fn save(surface: &mut ChatSurface, reply: MessageId, title: &str) {
    assert!(surface.perform_response_action(ResponseAction::Store(reply)));
    assert_eq!(
        surface.destination_open,
        Some(reply),
        "the name prompt opens"
    );
    surface.save_name.set_text(&mut surface.engine, title);
    assert!(surface.start_export(reply));
}

fn failure(retryable: bool) -> koru_ai_wire::Failure {
    koru_ai_wire::Failure {
        code: if retryable {
            "engine_unavailable"
        } else {
            "conflict"
        }
        .into(),
        message: "The document could not be saved.".into(),
        retryable,
        details: Default::default(),
    }
}

#[test]
fn only_published_bot_words_are_savable() {
    let (mut surface, _, _) = surface();
    let bot = message(5, "bot:synthetic");
    let person = message(6, "person:synthetic");
    let state = surface.managed.as_mut().unwrap();
    state.messages.insert(5, bot.clone());
    state.messages.insert(6, person.clone());
    assert_eq!(
        surface.bot_reply_sequence(MessageId::from_uuid(bot.id)),
        Some(5)
    );
    assert_eq!(
        surface.bot_reply_sequence(MessageId::from_uuid(person.id)),
        None
    );
    assert_eq!(surface.bot_reply_sequence(MessageId::new()), None);
}

#[test]
fn an_uncertain_save_is_resent_unchanged_and_a_refused_one_starts_over() {
    let (mut surface, commands, _) = surface();
    let bot = message(5, "bot:synthetic");
    let reply = MessageId::from_uuid(bot.id);
    let thread = surface.managed.as_ref().unwrap().thread();
    surface.managed.as_mut().unwrap().messages.insert(5, bot);

    save(&mut surface, reply, "  Launch plan ");
    let Command::Engine { command: first, .. } = commands.try_recv().unwrap() else {
        panic!("engine save")
    };
    let C::SaveReply {
        id,
        document,
        sequence,
        title,
        message,
        ..
    } = first.clone()
    else {
        panic!("save reply")
    };
    assert_eq!(
        (sequence, title.as_str(), message),
        (5, "Launch plan", reply.as_uuid())
    );
    assert!(surface.destination_open.is_none());

    // Uncertain: the same name sends the same request for the same page.
    assert!(surface.managed_failed(thread, first.clone(), failure(true)));
    assert!(surface.status.is_some());
    save(&mut surface, reply, "Launch plan");
    let Command::Engine {
        command:
            C::SaveReply {
                id: again,
                document: page,
                ..
            },
        ..
    } = commands.try_recv().unwrap()
    else {
        panic!("engine save")
    };
    assert_eq!((again, page), (id, document));

    // Refused: the next save is a new request.
    assert!(surface.managed_failed(thread, first, failure(false)));
    save(&mut surface, reply, "Launch plan");
    let Command::Engine {
        command:
            C::SaveReply {
                id: fresh,
                document: fresh_page,
                ..
            },
        ..
    } = commands.try_recv().unwrap()
    else {
        panic!("engine save")
    };
    assert_ne!(fresh, id);
    assert_ne!(fresh_page, document);

    // Saved: told as an ordinary save is, with its Open.
    assert!(surface.managed_reply(
        thread,
        R::Saved {
            request: fresh,
            document: fresh_page,
            workspace: Uuid::new_v4(),
            title: "Launch plan".into(),
        }
    ));
    assert_eq!(
        surface.status.as_deref(),
        Some("Stored “Launch plan” in Koru.")
    );
    assert!(surface.managed.as_ref().unwrap().save.is_none());
    // A late answer to a save no longer in flight changes nothing.
    surface.status = None;
    assert!(!surface.managed_reply(
        thread,
        R::Saved {
            request: id,
            document,
            workspace: Uuid::new_v4(),
            title: "Launch plan".into(),
        }
    ));
    assert!(surface.status.is_none());
}
