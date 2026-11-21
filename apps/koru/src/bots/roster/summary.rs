// SPDX-License-Identifier: MPL-2.0
//! Flat-row display and unread facts, independent of conversation activity.
use super::*;

pub(super) fn needs_attention(bot: &RosterBot, main_unread: bool) -> bool {
    main_unread
        || bot
            .summary
            .as_ref()
            .is_some_and(|s| s.pending_results > 0 || s.open_requests > 0)
}

pub(super) fn caption(bot: &RosterBot, lang: koru_i18n::Lang) -> String {
    let Some(summary) = &bot.summary else {
        return String::new();
    };
    if summary.open_requests > 0 {
        return ChatStatus::NeedsYou.spoken(lang).unwrap_or_default().into();
    }
    summary
        .latest
        .as_ref()
        .map(|m| m.text.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
}

pub(super) fn labels(bot: &RosterBot, selected: bool, lang: koru_i18n::Lang) -> El {
    let mut labels = col("Bot labels").fill().gap(Space::S1).child(
        text(&bot.name)
            .named("Bot name")
            .role(roles::ROW)
            .ink(if selected {
                Ink::Primary
            } else {
                Ink::Secondary
            })
            .emphasis(selected)
            .truncate(),
    );
    let preview = caption(bot, lang);
    if !preview.is_empty() {
        labels = labels.child(
            text(preview)
                .named("Bot last message")
                .role(roles::META)
                .ink(Ink::Hint)
                .truncate(),
        );
    }
    labels
}

impl BotsRoster {
    pub(crate) fn main_message_metadata(
        &self,
    ) -> impl Iterator<Item = (BotId, koru_ai_core::ChatId, i64)> + '_ {
        self.bots.iter().map(|bot| {
            let at = bot
                .summary
                .as_ref()
                .and_then(|s| s.latest.as_ref())
                .map_or(0, |m| m.at_ms);
            (bot.id, bot.main_chat, at)
        })
    }
}

#[cfg(test)]
mod tests;
