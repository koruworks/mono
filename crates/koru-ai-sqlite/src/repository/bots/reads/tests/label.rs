// SPDX-License-Identifier: MPL-2.0
//! The Bot label on a Chat: present only where the caller's proof admits
//! that Bot conversation, and a caller without it still sees no conversation.
use super::*;

fn label(bot: &Bot, purpose: BotChatPurpose) -> Option<BotChatLabel> {
    Some(BotChatLabel {
        bot_id: bot.id,
        bot_name: bot.config.name.clone(),
        purpose,
    })
}

#[test]
fn only_an_authorized_read_carries_the_label_and_nobody_else_sees_the_chat() {
    let (mut repo, bot, profile) = setup();
    let extra = additional(&mut repo, &bot, profile, 5);
    let ordinary = ChatId::new();
    repo.create_chat(&super::super::super::super::tests::chat(ordinary, 9))
        .unwrap();

    // Without the proof: neither Bot conversation exists, labelled or not.
    let none = ChatReadAccess::ordinary();
    assert_eq!(repo.chat_with_access(bot.main_chat, &none).unwrap(), None);
    assert_eq!(repo.chat_with_access(extra, &none).unwrap(), None);
    let listed = repo.chats_with_access(&none).unwrap();
    assert_eq!(
        listed.iter().map(|c| c.id).collect::<Vec<_>>(),
        vec![ordinary]
    );
    assert!(listed.iter().all(|c| c.bot.is_none()));

    // A proof for the main conversation only: it is labelled; the other Bot
    // conversation stays invisible; the ordinary chat is never labelled.
    let main_only = ChatReadAccess::authorized(vec![bot.main_chat]);
    assert_eq!(
        repo.chat_with_access(bot.main_chat, &main_only)
            .unwrap()
            .unwrap()
            .bot,
        label(&bot, BotChatPurpose::Main)
    );
    assert_eq!(repo.chat_with_access(extra, &main_only).unwrap(), None);
    let listed = repo.chats_with_access(&main_only).unwrap();
    let mut seen: Vec<_> = listed.iter().map(|c| (c.id, c.bot.clone())).collect();
    seen.sort_by_key(|(id, _)| id.to_storage());
    let mut expected = vec![
        (bot.main_chat, label(&bot, BotChatPurpose::Main)),
        (ordinary, None),
    ];
    expected.sort_by_key(|(id, _)| id.to_storage());
    assert_eq!(seen, expected);

    // Both admitted: each carries its own purpose.
    let both = ChatReadAccess::authorized(vec![bot.main_chat, extra]);
    assert_eq!(
        repo.chat_with_access(extra, &both).unwrap().unwrap().bot,
        label(&bot, BotChatPurpose::Additional)
    );
    // An ordinary chat named in a proof still has no label, and its JSON
    // has no `bot` key at all, so older readers see what they always saw.
    let plain = repo
        .chat_with_access(ordinary, &ChatReadAccess::authorized(vec![ordinary]))
        .unwrap()
        .unwrap();
    assert_eq!(plain.bot, None);
    assert!(serde_json::to_value(&plain).unwrap().get("bot").is_none());
}

#[test]
fn a_page_entry_carries_the_same_label_as_its_chat() {
    let (mut repo, bot, profile) = setup();
    additional(&mut repo, &bot, profile, 5);
    let page = repo.bot_chat_page(bot.id, None, 10).unwrap();
    assert!(
        page.chats
            .iter()
            .all(|entry| entry.chat.bot.as_ref() == Some(&entry.label))
    );
}
