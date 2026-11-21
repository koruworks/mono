// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::managed_surface::tests::surface;
use koru_ai_core::bots::BotReplyPreview;

#[test]
fn reloaded_reply_links_to_retained_result_and_never_opens_a_thread() {
    let (mut view, commands, _) = surface();
    let state = view.managed.as_mut().unwrap();
    let bot = state.snapshot.conversation.label.bot_id;
    let occurrence = Uuid::new_v4();
    let card = Uuid::new_v4();
    let reply = Uuid::new_v4();
    let mut message = crate::managed::tests::message(3, Uuid::new_v4());
    message.sender = "person:fixture".into();
    message.content.in_reply_to = Some(card);
    let person = MessageId::from_uuid(message.id);
    state.ingest(message);
    state.navigation = State::new(Page {
        items: vec![wire::Card {
            id: card,
            sequence: 2,
            message: card,
            kind: "thread".into(),
            title: "Routine".into(),
            thread: Some(ChatId::new()),
            bot: Some(bot),
            bot_name: "Scout".into(),
            workspace: None,
            document: None,
            messages: 1,
            outputs: 0,
            state: "active".into(),
            dispatches: vec![],
            routine: Some(wire::RoutineResult {
                routine: Uuid::new_v4(),
                occurrence,
                at_ms: 1,
                manual: false,
                reply,
                preview: BotReplyPreview::new(
                    MessageId::from_uuid(reply),
                    "The original result\nLong body",
                ),
            }),
        }],
        ..Default::default()
    });
    view.transcript = Some(state.transcript());
    view.rebuild_messages(420.0);
    let index = view
        .navigation()
        .rows
        .iter()
        .position(|r| r.id == person.as_uuid())
        .unwrap();
    let row = &view.navigation().rows[index];
    assert_eq!(row.heading, "The original result");
    assert_eq!(row.before, Some(person));
    assert!(row.targets.is_empty());
    assert!(row.height > 0.0 && row.height < 50.0);
    view.review = Default::default();
    assert!(view.activate_managed_navigation(index));
    assert!(view.take_open_bot_conversation().is_none());
    assert!(
        commands
            .try_iter()
            .any(|c| matches!(c, Command::RoutineReview {
        command: koru_ai_wire::bot_routines::Command::Review { occurrence: actual, .. }, ..
    } if actual == occurrence))
    );
}
