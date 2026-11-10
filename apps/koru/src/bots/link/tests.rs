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
