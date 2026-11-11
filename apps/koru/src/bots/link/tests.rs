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
        listed(Err(Said::new("wrong_owner", false))),
        Err(Failure::Refused("wrong_owner".to_owned()))
    );
    assert_eq!(listed(Err(Said::new("", false))), Err(Failure::Unanswered));
    let other = BotResponse::Chat {
        reference: BotChatReference {
            bot_id: BotId::mint(),
            chat_id: ChatId::new(),
        },
    };
    assert_eq!(listed(Ok(other)), Err(Failure::Unanswered));
}

/// **What leaves the outcome unknown, and what is a refusal**, as the
/// client, the gateway and the AI worker say it: each row is a code and
/// flag one of them produces (named beside it), and what every surface
/// reads it as. A lost answer is never a refusal, so no write whose
/// outcome is unknown is ever said to be not applied.
#[test]
fn every_failure_the_client_reports_is_classified_once() {
    let unanswered = [
        // `koru_api_client::transport_failure` and `gateway_failure`: the
        // connection ended, or discovery failed.
        ("gateway_unavailable", true),
        // The client with no gateway session; the gateway with no AI
        // client or a lost task; the worker gone after the command queued.
        ("ai_unavailable", true),
        // `receive_api`: the writer did not answer in time.
        ("ai_worker_timeout", true),
        // `authorize`, and the gateway's own worker timeout.
        ("deadline_exceeded", true),
        // `protocol_failure`: the reply did not parse.
        ("invalid_response", true),
        // `checked`: a 5xx whose body did not parse.
        ("gateway_failure", true),
        // No code at all.
        ("", false),
    ];
    for (code, retryable) in unanswered {
        let failure = Failure::of(Said::new(code, retryable));
        assert_eq!(failure, Failure::Unanswered, "{code}");
        assert!(failure.again(), "{code}: asked again");
    }
    let refused = [
        // `queue_failure`: the writer's queue was full; never queued.
        ("ai_worker_saturated", true, true),
        // `bot_failure`: Unavailable, Storage, Settling.
        ("workspace_unavailable", true, true),
        ("storage_error", true, true),
        ("bot_operation_pending", true, true),
        // The execution owner did not answer a read.
        ("bot_unavailable", true, true),
        // `bot_failure`: the rest, each final.
        ("request_conflict", false, false),
        ("revision_conflict", false, false),
        ("not_found", false, false),
        ("wrong_owner", false, false),
        ("invalid_request", false, false),
        ("folder_locked", false, false),
        // `hold_failure` and `archived_failure`.
        ("bot_lifecycle_pending", false, false),
        ("bot_conversation_trashed", false, false),
        ("bot_trashed", false, false),
        ("bot_archived", false, false),
        // `checked`: a 4xx whose body did not parse, and a 401.
        ("gateway_failure", false, false),
        ("unauthenticated", false, false),
    ];
    for (code, retryable, again) in refused {
        let failure = Failure::of(Said::new(code, retryable));
        assert_eq!(failure, Failure::Refused(code.to_owned()), "{code}");
        assert_eq!(failure.again(), again, "{code}");
    }
}

/// Every reply helper classifies through the one function: a lost
/// connection is unanswered on a read, a list, a write and a label alike.
#[test]
fn every_reply_reads_a_lost_connection_as_unanswered() {
    let lost = || Said::new("gateway_unavailable", true);
    assert_eq!(read(Err(lost())), Err(Failure::Unanswered));
    assert_eq!(listed(Err(lost())), Err(Failure::Unanswered));
    assert_eq!(wrote(Err(lost())), Err(Failure::Unanswered));
    assert_eq!(labelled(Err(lost())), Err(Failure::Unanswered));
    assert_eq!(chats(Err(lost())), Err(Failure::Unanswered));
    assert_eq!(activity(Err(lost())), Err(Failure::Unanswered));
    assert_eq!(unfinished(|_| Err(lost())), Err(Failure::Unanswered));
    assert_eq!(trashed_chats(|_| Err(lost())), Err(Failure::Unanswered));
    assert_eq!(Failure::lost(), Failure::Unanswered);
    assert_eq!(Failure::timed_out(), Failure::Unanswered);
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
