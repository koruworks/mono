// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_ai_core::ChatId;
use koru_ai_core::bots::{BotChatReference, BotId};

#[test]
fn a_list_reply_is_the_roster() {
    let home = WorkspaceId::new();
    let one = crate::bots::tests::bot(BotId::mint(), home, "Scout", false);
    assert_eq!(
        listed(Ok(BotResponse::List {
            bots: vec![one.clone()]
        })),
        Ok(vec![one])
    );
    assert_eq!(
        listed(Ok(BotResponse::List { bots: Vec::new() })),
        Ok(Vec::new())
    );
}

/// **A failure is never an empty roster**: a refusal keeps its code, and a
/// reply of the wrong shape or a refusal with no code is *not answering*.
#[test]
fn nothing_but_a_list_reads_as_a_list() {
    assert_eq!(
        listed(Err("wrong_owner".to_owned())),
        Err(Failure::Refused("wrong_owner".to_owned()))
    );
    assert_eq!(listed(Err(String::new())), Err(Failure::Unreachable));
    let other = BotResponse::Chat {
        reference: BotChatReference {
            bot_id: BotId::mint(),
            chat_id: ChatId::new(),
        },
    };
    assert_eq!(listed(Ok(other)), Err(Failure::Unreachable));
}

/// The link asks for Koru AI's chat scope and the Bot owner's, and holds
/// nothing else: naming scopes filters rather than adds.
#[test]
fn the_link_holds_exactly_the_two_scopes_it_needs() {
    assert_eq!(SCOPES, ["ai.chat", "bots.manage"]);
}

/// **No thread under test.** A suite that started one would dial the gateway
/// of whatever Koru the developer has running.
#[test]
fn the_link_never_starts_under_test() {
    assert!(BotsLink::start(None).is_none());
}

/// **A bounded listing never cuts silently**: the owner's Trash listing
/// (`list_trashed_bot_chats`, a `next_cursor` on every page but the last)
/// read to its page limit with a page still owed says so; one that ends
/// within it says nothing is left.
#[test]
fn a_listing_that_stops_at_its_page_limit_says_more_are_left() {
    use koru_ai_core::bots::{
        BotChatRestore, BotLifecycleKind, BotRequestId, BotTrashedChatCursor, BotTrashedChatPage,
    };
    let page = |last: bool| {
        let chat = ChatId::new();
        Ok(BotResponse::TrashedChats {
            page: BotTrashedChatPage {
                chats: vec![crate::bots::tests::trashed_chat(
                    BotId::mint(),
                    "Writer",
                    chat,
                    "Notes",
                    (BotRequestId::mint(), BotLifecycleKind::TrashChat),
                    BotChatRestore::Conversation,
                )],
                next_cursor: (!last).then_some(BotTrashedChatCursor { chat_id: chat }),
            },
        })
    };
    let mut asked = 0;
    let endless = trashed_chats(|_| {
        asked += 1;
        page(false)
    })
    .expect("listed");
    assert_eq!(asked, CREATION_PAGES, "never past the limit");
    assert_eq!(endless.items.len(), CREATION_PAGES);
    assert!(endless.more, "pages were left unread");
    let mut asked = 0;
    let short = trashed_chats(|_| {
        asked += 1;
        page(asked == 2)
    })
    .expect("listed");
    assert_eq!(short.items.len(), 2);
    assert!(!short.more, "the owner's last page was read");
}
