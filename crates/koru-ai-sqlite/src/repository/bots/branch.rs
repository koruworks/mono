// SPDX-License-Identifier: MPL-2.0

//! Branch into another conversation with the same Bot, and the inert Run
//! history a copied reply still points at.
use super::*;
use koru_ai_core::MessageId;

/// Hops followed through `origin_message_id`. A longer chain resolves nothing
/// past this bound, and a damaged cyclic chain stops here instead of looping.
const MAX_ORIGIN_HOPS: u32 = 32;

/// The nearest reply a Bot Run wrote, from `?1` back through the copies
/// Branch made, at most `?2` hops: each hop a `messages` primary-key seek,
/// each candidate a seek of the unique `bot_chat_runs.assistant_message_id`.
pub(super) const HISTORICAL_BOT_RUN_SQL: &str = "WITH RECURSIVE chain(id, hops) AS (
                   SELECT ?1, 0
                   UNION ALL
                   SELECT m.origin_message_id, chain.hops + 1 FROM messages m
                     JOIN chain ON m.id = chain.id
                    WHERE m.origin_message_id IS NOT NULL AND chain.hops < ?2)
                 SELECT r.bot_id, r.chat_id, r.assistant_message_id, r.run_id
                   FROM chain JOIN bot_chat_runs r ON r.assistant_message_id = chain.id
                  ORDER BY chain.hops LIMIT 1";

/// The Run that produced a reply, found from that reply or any copy of it.
/// History only: it confers no control, approval or result authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoricalBotRun {
    pub bot: BotId,
    pub chat: ChatId,
    pub assistant_message: MessageId,
    pub run: koru_delegate_core::RunId,
}

impl Repository {
    /// Fail every Bot branch commit at its last statement, the association
    /// insert, so the whole copy rolls back; `false` lifts it.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn refuse_bot_branch_commit_for_fixture(&self, refuse: bool) -> Result<(), BotError> {
        self.connection
            .execute_batch(if refuse {
                "CREATE TRIGGER refuse_bot_branch BEFORE INSERT ON bot_chats WHEN NEW.purpose='additional' BEGIN SELECT RAISE(ABORT,'fixture branch rollback'); END;"
            } else {
                "DROP TRIGGER refuse_bot_branch;"
            })
            .map_err(bad)
    }

    /// The shared branch copy and its `additional` association commit together.
    /// The caller has checked current authority and built the copy from the
    /// source's terminal prefix; this rechecks the Bot snapshot, that the source
    /// is this Bot's and that the destination does not exist, in the same
    /// transaction. It copies no tool activity, attempt, Run or replay row.
    pub fn create_bot_branch(&mut self, bot: &Bot, branch: &BranchCopy) -> Result<(), BotError> {
        validate_branch(branch).map_err(|_| BotError::Invalid)?;
        let origin = branch
            .chat
            .branched_from
            .as_ref()
            .ok_or(BotError::Invalid)?;
        let tx = self.connection.transaction().map_err(bad)?;
        fence_bot(&tx, bot)?;
        if bot_for_chat_on(&tx, origin.chat_id)? != Some(bot.id) {
            return Err(BotError::Forbidden);
        }
        let destination = ChatReadAccess::authorized(vec![branch.chat.id]);
        if query_chat_with_access(&tx, branch.chat.id, &destination)
            .map_err(|_| BotError::Storage)?
            .is_some()
        {
            return Err(BotError::Conflict);
        }
        insert_branch_on(&tx, branch).map_err(|_| BotError::Storage)?;
        insert_additional_chat(&tx, branch.chat.id, bot.id, branch.chat.created_at)?;
        tx.commit().map_err(bad)?;
        Ok(())
    }

    /// Follow a message back through the copies Branch made of it to the reply a
    /// Bot Run wrote. The nearest recorded reply wins. A purged origin ends the
    /// chain, so a surviving branch resolves nothing after its source is erased.
    pub fn historical_bot_run(
        &self,
        message: MessageId,
    ) -> Result<Option<HistoricalBotRun>, BotError> {
        let row: Option<(String, String, String, String)> = self
            .connection
            .query_row(
                HISTORICAL_BOT_RUN_SQL,
                params![message.to_storage(), MAX_ORIGIN_HOPS],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(bad)?;
        row.map(|(bot, chat, assistant, run)| {
            Ok(HistoricalBotRun {
                bot: bot.parse().map_err(bad)?,
                chat: ChatId::parse(&chat).ok_or(BotError::Storage)?,
                assistant_message: MessageId::parse(&assistant).ok_or(BotError::Storage)?,
                run: run.parse().map_err(bad)?,
            })
        })
        .transpose()
    }
}
