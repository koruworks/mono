// SPDX-License-Identifier: MPL-2.0

//! **Whether an Archive or a Trash will stop work running now** (Bots P1.8,
//! QUESTIONS O46, the owner's decision of 2026-11-11).
//!
//! A Bot or a conversation comes back from Archive or the Trash, but a reply
//! it was running is stopped for good. So the question asked before an
//! Archive, a Trash or a conversation's Trash has **Cancel as its default
//! when work is running now**, and says so; when nothing is running it is as
//! it always was, Enter confirming (ADR-265's row for a reversible action).
//!
//! **What the window already knows, at the moment of asking** — no request
//! is sent to find out:
//!
//! - **the asking tab's own conversation**: its status as the window's pass
//!   last set it from the chat surface (`AiChatView::status`: a reply
//!   running, or one waiting on the person). It is evidence that work runs,
//!   never that none does — a surface that has not read its transcript yet
//!   also says idle;
//! - **the roster's activity read** (`ListActivity`, the running and
//!   Needs-you conversations of the workspace), only while it is fresh
//!   (`BotsRoster::activity_now`): answered, nothing owed or on its way
//!   since, the roster shown throughout, and no older than
//!   [`crate::bots::roster::ACTIVITY_FRESH`]. Only a fresh read that went
//!   to the end can say nothing is running.
//!
//! "Needs you" counts as running: a reply waiting on an approval is work the
//! Archive or Trash stops too. Anything the window does not know is
//! [`WorkNow::Unknown`], and the default is the safe one, Cancel.

use koru_ai_core::ChatId;
use koru_ai_core::bots::{BotActivity, BotActivityState, BotId};
use koru_chats_ui::ChatStatus;

/// What an Archive or a Trash would stop, as far as the window knows.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkNow {
    /// A reply is running.
    Running,
    /// A reply is waiting on the person (an approval, a permission).
    NeedsYou,
    /// Known: nothing is running.
    Idle,
    /// The window does not know.
    Unknown,
}

impl WorkNow {
    /// **Whether the question's default is Cancel**: anything but a known
    /// idle.
    pub(crate) fn cancel_by_default(self) -> bool {
        self != Self::Idle
    }
}

/// What the question is about.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    /// The whole Bot: an Archive or a Trash stops any of its conversations.
    Bot(BotId),
    /// One conversation's Trash stops that conversation only.
    Conversation(BotId, ChatId),
}

/// What the window knows, gathered at the moment of asking.
#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct Facts<'a> {
    /// The asking tab's own conversation and its status as last set.
    pub own: Option<(ChatId, ChatStatus)>,
    /// The roster's activity read **while fresh**: its entries, and whether
    /// it stopped before the end. `None` when not read yet, or stale.
    pub activity: Option<(&'a [BotActivity], bool)>,
}

/// **What an Archive or a Trash of `scope` would stop**, from `facts`.
pub(crate) fn work_now(scope: Scope, facts: Facts<'_>) -> WorkNow {
    let (bot, chat) = match scope {
        Scope::Bot(bot) => (bot, None),
        Scope::Conversation(bot, chat) => (bot, Some(chat)),
    };
    let covers = |of: ChatId| chat.is_none_or(|chat| chat == of);
    let (mut running, mut needs_you) = (false, false);
    if let Some((own, status)) = facts.own
        && covers(own)
    {
        match status {
            ChatStatus::Working => running = true,
            ChatStatus::NeedsYou => needs_you = true,
            // Not evidence that nothing runs: see the module docs.
            ChatStatus::Idle | ChatStatus::Unread => {}
        }
    }
    let mut known = false;
    if let Some((entries, truncated)) = facts.activity {
        for entry in entries
            .iter()
            .filter(|entry| entry.bot_id == bot && covers(entry.chat_id))
        {
            match entry.state {
                BotActivityState::Running => running = true,
                BotActivityState::NeedsYou => needs_you = true,
            }
        }
        // A read that stopped before the end cannot say what it left out.
        known = !truncated;
    }
    if running {
        WorkNow::Running
    } else if needs_you {
        WorkNow::NeedsYou
    } else if known {
        WorkNow::Idle
    } else {
        WorkNow::Unknown
    }
}

#[cfg(test)]
mod tests;
