// SPDX-License-Identifier: MPL-2.0

use super::*;

fn entry(bot: BotId, chat: ChatId, state: BotActivityState) -> BotActivity {
    BotActivity {
        waiting: None,
        bot_id: bot,
        chat_id: chat,
        state,
    }
}

/// **The activity read decides a whole Bot**: any of its conversations
/// running or waiting on the person; idle only when the fresh read went to
/// the end and names none of them; unknown when it is stale or stopped
/// short. Another Bot's work is not this one's.
#[test]
fn a_whole_bot_is_running_waiting_idle_or_unknown_as_the_fresh_read_says() {
    let (bot, other) = (BotId::mint(), BotId::mint());
    let (chat, second) = (ChatId::new(), ChatId::new());
    let read = |entries: &[BotActivity], truncated: bool| {
        work_now(
            Scope::Bot(bot),
            Facts {
                own: None,
                activity: Some((entries, truncated)),
            },
        )
    };
    assert_eq!(
        read(&[entry(bot, second, BotActivityState::Running)], false),
        WorkNow::Running
    );
    assert_eq!(
        read(&[entry(bot, second, BotActivityState::NeedsYou)], false),
        WorkNow::NeedsYou
    );
    assert_eq!(
        read(
            &[
                entry(bot, chat, BotActivityState::NeedsYou),
                entry(bot, second, BotActivityState::Running),
            ],
            false
        ),
        WorkNow::Running,
        "running is said over waiting"
    );
    assert_eq!(
        read(&[entry(other, chat, BotActivityState::Running)], false),
        WorkNow::Idle
    );
    assert_eq!(read(&[], true), WorkNow::Unknown, "stopped short");
    assert_eq!(
        work_now(Scope::Bot(bot), Facts::default()),
        WorkNow::Unknown
    );
    assert!(WorkNow::Unknown.cancel_by_default());
    assert!(WorkNow::Running.cancel_by_default());
    assert!(WorkNow::NeedsYou.cancel_by_default());
    assert!(!WorkNow::Idle.cancel_by_default());
}

/// **A conversation's Trash is about that conversation alone**, and the
/// tab's own status is evidence that work runs, never that none does.
#[test]
fn a_conversation_is_about_itself_and_the_tabs_idle_proves_nothing() {
    let bot = BotId::mint();
    let (chat, second) = (ChatId::new(), ChatId::new());
    let busy = [entry(bot, second, BotActivityState::Running)];
    let fresh = |own| Facts {
        own,
        activity: Some((&busy, false)),
    };
    assert_eq!(
        work_now(Scope::Conversation(bot, chat), fresh(None)),
        WorkNow::Idle,
        "the other conversation runs, not this one"
    );
    assert_eq!(
        work_now(Scope::Conversation(bot, second), fresh(None)),
        WorkNow::Running
    );
    // The tab's own conversation, with no fresh read, not known idle.
    let own = |status| Facts {
        own: Some(Own {
            chat,
            status,
            known_idle: false,
        }),
        activity: None,
    };
    assert_eq!(
        work_now(Scope::Conversation(bot, chat), own(ChatStatus::Working)),
        WorkNow::Running
    );
    assert_eq!(
        work_now(Scope::Bot(bot), own(ChatStatus::NeedsYou)),
        WorkNow::NeedsYou
    );
    assert_eq!(
        work_now(Scope::Conversation(bot, chat), own(ChatStatus::Idle)),
        WorkNow::Unknown,
        "a surface that has not read its transcript says idle too"
    );
    assert_eq!(
        work_now(Scope::Conversation(bot, second), own(ChatStatus::Working)),
        WorkNow::Unknown,
        "another conversation's status says nothing of this one"
    );
}

/// **The tab's own conversation, read and idle, is known idle — for that
/// conversation's question only** (the fourth window run: the question for
/// an idle conversation said its running work is stopped whenever the
/// roster's activity read was more than 30 s old). Work the roster's fresh
/// read names still wins, and a whole Bot's question learns nothing from it.
#[test]
fn a_tab_that_read_its_conversation_and_finds_it_idle_knows_it_is_idle() {
    let bot = BotId::mint();
    let (chat, second) = (ChatId::new(), ChatId::new());
    let read_idle = Some(Own {
        chat,
        status: ChatStatus::Idle,
        known_idle: true,
    });
    let alone = Facts {
        own: read_idle,
        activity: None,
    };
    assert_eq!(
        work_now(Scope::Conversation(bot, chat), alone),
        WorkNow::Idle
    );
    assert_eq!(
        work_now(Scope::Conversation(bot, second), alone),
        WorkNow::Unknown,
        "another conversation"
    );
    assert_eq!(
        work_now(Scope::Bot(bot), alone),
        WorkNow::Unknown,
        "the whole Bot needs the roster"
    );
    let busy = [entry(bot, chat, BotActivityState::Running)];
    assert_eq!(
        work_now(
            Scope::Conversation(bot, chat),
            Facts {
                own: read_idle,
                activity: Some((&busy, false)),
            },
        ),
        WorkNow::Running,
        "the owner's fresh word that it runs wins"
    );
}
