// SPDX-License-Identifier: MPL-2.0
//! Lookup-only trusted tool inputs. These values are not current Run authority.
use super::*;

#[derive(Clone, PartialEq, Eq)]
pub struct RoutineToolSubject {
    pub metadata: BotPolicyMetadata,
    pub profile: ProfileId,
    pub receipt: BotSubmissionReceipt,
    pub occurrence: Option<BotRoutineStoredPreparation>,
}
impl std::fmt::Debug for RoutineToolSubject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoutineToolSubject([REDACTED])")
    }
}

impl Repository {
    fn routine_tool_metadata(
        &self,
        owner: BotOwner,
        bot: BotId,
    ) -> Result<BotPolicyMetadata, BotError> {
        let metadata = self.bot_policy_metadata(owner, bot)?;
        if metadata.lifecycle == BotLifecycle::Trashed {
            return Err(BotError::NotFound);
        }
        if metadata.pending_lifecycle.is_some() {
            return Err(BotError::Unavailable);
        }
        Ok(metadata)
    }

    /// Exact full retained receipt and, for Routine provenance, the sole Linked
    /// decoder. Current execution settings are not an observation prerequisite.
    /// The worker independently authenticates generation/human rights and repeats
    /// this complete snapshot after external work. Listing scalars are not used.
    pub fn routine_tool_subject(
        &self,
        owner: BotOwner,
        bot: BotId,
        run: koru_delegate_core::RunId,
    ) -> Result<RoutineToolSubject, BotError> {
        let metadata = self.routine_tool_metadata(owner, bot)?;
        let profile: String = self.connection.query_row(
            "SELECT CASE WHEN typeof(profile_id)='text' AND length(CAST(profile_id AS BLOB))=36 THEN profile_id END FROM chats WHERE id=?1",
            [metadata.main_chat.to_storage()], |r| r.get(0),
        ).map_err(bad)?;
        let profile = ProfileId::parse(&profile).ok_or(BotError::Storage)?;
        // The ordinary decoder owns syntax/identity. Bound its raw allocations
        // before calling it here, including malformed rows beyond SQL CHECKs.
        let (count, bounded): (i64, Option<bool>) = self.connection.query_row(
            "SELECT count(*),min(typeof(request_id)='text' AND length(CAST(request_id AS BLOB))=36
             AND typeof(fingerprint)='text' AND length(CAST(fingerprint AS BLOB))=64
             AND typeof(command_kind)='text' AND length(CAST(command_kind AS BLOB))<=32
             AND typeof(stage)='text' AND length(CAST(stage AS BLOB))<=16
             AND typeof(submission_json)='text' AND length(CAST(submission_json AS BLOB)) BETWEEN 1 AND 4194304
             AND (outcome_json IS NULL OR (typeof(outcome_json)='text' AND length(CAST(outcome_json AS BLOB))<=1048576))
             AND (pause_ack_stage IS NULL OR (typeof(pause_ack_stage)='text' AND length(CAST(pause_ack_stage AS BLOB))<=16)))
             FROM bot_command_receipts WHERE user_scope_id=?1 AND bot_id=?2 AND planned_run_id=?3",
            params![owner.user_scope.to_storage(),bot.to_storage(),run.to_storage()],
            |r| Ok((r.get(0)?,r.get(1)?)),
        ).map_err(bad)?;
        if count == 0 {
            return Err(BotError::NotFound);
        }
        if count != 1 || bounded != Some(true) {
            return Err(BotError::Storage);
        }
        let receipt = admission::readable_submission_for_run_on(&self.connection, owner, bot, run)?
            .ok_or(BotError::NotFound)?;
        let submission = &receipt.intent.submission;
        if submission.workspace != metadata.home_workspace {
            return Err(BotError::Forbidden);
        }
        if let Some(id) = submission.provenance.ai_attempt_id {
            let attempt = InferenceAttemptId::parse(&id.to_string()).ok_or(BotError::Storage)?;
            crate::repository::bot_replay::validate_link(&self.connection, attempt, submission)
                .map_err(|_| BotError::Storage)?;
        }
        let occurrence = if let Some(subject) = submission.provenance.routine {
            let row = occurrence::for_run_on(&self.connection, owner, bot, run)?
                .ok_or(BotError::NotFound)?;
            if row.prepared.submission != *submission
                || row.prepared.slot.schedule != subject.schedule
                || row.capture.canonical.home != metadata.home_workspace
            {
                return Err(BotError::Storage);
            }
            Some(row)
        } else {
            None
        };
        Ok(RoutineToolSubject {
            metadata,
            profile,
            receipt,
            occurrence,
        })
    }

    /// Receipt/kind lookup and full bounded identity validation precede all
    /// fresh work. Return the actual retained outcome rather than rebuilding it.
    pub fn routine_tool_creation_by_request(
        &self,
        owner: BotOwner,
        bot: BotId,
        request: BotRequestId,
    ) -> Result<Option<(BotRoutineCreation, BotRoutineCreationReceipt)>, BotError> {
        let metadata = self.routine_tool_metadata(owner, bot)?;
        let Some(receipt) = bounded::receipt(&self.connection, owner, bot, request)? else {
            return Ok(None);
        };
        let creation = bounded::by_request(&self.connection, owner, bot, request, &receipt)?;
        if creation.identity.home != metadata.home_workspace {
            return Err(BotError::Forbidden);
        }
        Ok(Some((creation, receipt.outcome)))
    }
}
