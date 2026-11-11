// SPDX-License-Identifier: MPL-2.0

use super::*;

fn entry(bot: BotId, chat: ChatId, state: BotActivityState) -> BotActivity {
    BotActivity {
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
    // The tab's own conversation, with no fresh read.
    let own = |status| Facts {
        own: Some((chat, status)),
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
