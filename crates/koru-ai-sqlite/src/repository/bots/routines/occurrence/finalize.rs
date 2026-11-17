// SPDX-License-Identifier: MPL-2.0
use super::*;

fn captured(row: &BotRoutineStoredPreparation, profile: ProfileId) -> BotRoutineCapturedSources {
    let c = &row.capture;
    BotRoutineCapturedSources {
        slot: row.prepared.slot,
        definition: c.schedule_definition.clone(),
        owner_handoff: row.prepared.definition.owner_handoff,
        creation: c.creation.clone(),
        canonical: c.canonical.clone(),
        profile,
        grant: c.grant.clone(),
        role: c.role.clone(),
        knowledge: c.library.knowledge.clone(),
        task: c.task.clone(),
        memory: c.library.memory.clone(),
        index_digest: c.library.index_digest,
    }
}

impl Repository {
    /// Independent claim truth is obtained off-writer. This transaction compares
    /// that complete winner and never trusts an HTTP caller's claim assertion.
    pub fn finalize_routine_occurrence(
        &mut self,
        record: &RoutineClaimRecord,
        authority: &BotRoutineFinalizeAuthority,
        prompt_template_version: &str,
        now: i64,
    ) -> Result<BotRoutinePreparationAdmission, BotError> {
        if !record.valid() {
            return Err(BotError::Invalid);
        }
        let slot = &record.claim.slot;
        if authority.owner != slot.owner {
            return Err(BotError::Forbidden);
        }
        let tx = self.connection.transaction().map_err(bad)?;
        visible(&tx, owner(slot), slot.bot)?;
        let mut row = rows::read(&tx, slot)?.ok_or(BotError::NotFound)?;
        if !record.claim.matches_prepared(&row.prepared)
            || record.submission != row.prepared.submission
        {
            return Err(BotError::Conflict);
        }
        if authority.home != row.capture.canonical.home {
            return Err(BotError::Forbidden);
        }
        // Immutable materialization/closure wins over current configuration.
        if row.stage == BotRoutineStoredStage::Linked {
            if let RoutineLinkState::Terminal { reason } = record.link_state {
                settle_linked(&tx, &row, reason, now)?;
                tx.commit().map_err(bad)?;
            }
            return Ok(BotRoutinePreparationAdmission::Ready(Box::new(row)));
        }
        if let BotRoutineStoredStage::Closed(_) = row.stage {
            return Ok(BotRoutinePreparationAdmission::Ready(Box::new(row)));
        }
        let refusal = if let RoutineLinkState::Terminal { reason } = record.link_state {
            Some(reason)
        } else {
            // A delegate Linked record without AI materialization contradicts
            // the protocol; never manufacture the missing acknowledged Chat.
            if record.link_state != (RoutineLinkState::Held {}) {
                return Err(BotError::Conflict);
            }
            let (current, profile) = canonical(&tx, slot)?;
            let expected = match row.capture.schedule_definition.template.requested_by {
                koru_delegate_core::ActorRef::Person { profile } => profile,
                _ => return Err(BotError::Storage),
            };
            if profile != authority.profile {
                return Err(BotError::Forbidden);
            }
            if profile != expected {
                Some(RoutineTerminalReason::AuthorityChanged)
            } else if current != row.capture.canonical {
                Some(RoutineTerminalReason::ConfigurationChanged)
            } else {
                match fresh(&tx, &captured(&row, profile)) {
                    Ok(()) => None,
                    Err(BotError::RevisionConflict | BotError::Conflict) => {
                        Some(RoutineTerminalReason::ConfigurationChanged)
                    }
                    Err(error) => return Err(error),
                }
            }
        };
        if let Some(reason) = refusal {
            rows::close(
                &tx,
                &mut row,
                BotRoutineStoredClosure::ClaimedTerminal { reason },
                now,
            )?;
            tx.commit().map_err(bad)?;
            return Ok(BotRoutinePreparationAdmission::Ready(Box::new(row)));
        }
        let p = &row.prepared;
        let count:u32=tx.query_row("SELECT COUNT(*) FROM (SELECT 1 FROM bot_command_receipts WHERE user_scope_id=?1 AND bot_id=?2 AND command_kind IN ('start','continue','routine_occurrence') AND stage='prepared' LIMIT 51)",params![slot.owner.user_scope.to_storage(),slot.bot.to_storage()],|r|r.get(0)).map_err(bad)?;
        if count >= 50 {
            return Ok(BotRoutinePreparationAdmission::Capacity);
        }
        let (frozen,lock):(Option<i64>,Option<String>)=tx.query_row("SELECT folder_frozen_at_ms,CASE WHEN folder_lock_request_id IS NULL OR (typeof(folder_lock_request_id)='text' AND length(CAST(folder_lock_request_id AS BLOB))=36) THEN folder_lock_request_id ELSE 1 END FROM bots WHERE bot_id=?1",[slot.bot.to_storage()],|r|Ok((r.get(0)?,r.get(1)?))).map_err(bad)?;
        if frozen.is_none() {
            if lock
                .as_deref()
                .is_some_and(|key| key != p.ids.submission.to_storage())
            {
                return Err(BotError::FolderLocked);
            }
            let pending:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM bot_command_receipts WHERE user_scope_id=?1 AND bot_id=?2 AND command_kind IN ('start','continue','routine_occurrence') AND stage IN ('prepared','accepted'))",params![slot.owner.user_scope.to_storage(),slot.bot.to_storage()],|r|r.get(0)).map_err(bad)?;
            if pending {
                return Err(BotError::FolderLocked);
            }
            tx.execute(
                "UPDATE bots SET folder_lock_request_id=?2 WHERE bot_id=?1",
                params![slot.bot.to_storage(), p.ids.submission.to_storage()],
            )
            .map_err(bad)?;
            crate::repository::bots::client_feed::touch_bot(&tx, slot.bot).map_err(bad)?;
        }
        let stamp = Timestamp::from_millis(now.max(row.updated_at_ms));
        let config = &row.capture.canonical.config;
        let chat = Chat {
            id: ChatId::from_uuid(p.ids.chat),
            title: koru_ai_core::NEW_CHAT_TITLE.to_owned(),
            private: false,
            current_model: config.model.clone(),
            branched_from: None,
            agent_working_folder: Some(config.working_folder.clone()),
            agent_mode: config.mode.clone(),
            agent_fast: config.fast,
            pinned_at: None,
            profile_id: Some(authority.profile),
            bot: None,
            created_at: stamp,
            updated_at: stamp,
        };
        insert_chat(&tx, &chat).map_err(|_| BotError::Storage)?;
        insert_additional_chat(&tx, chat.id, slot.bot, stamp)?;
        let intent = BotSubmissionIntent {
            version: 2,
            submission: p.submission.clone(),
        };
        let changed=tx.execute("UPDATE bot_command_receipts SET command_kind='routine_occurrence',stage='prepared',planned_run_id=?3,captured_config_revision=?4,captured_policy_revision=?5,submission_version=2,submission_json=?6,outcome_json=NULL,updated_at_ms=?7 WHERE user_scope_id=?1 AND request_id=?2 AND command_kind='routine_draft' AND stage='prepared' AND fingerprint=?8",
            params![slot.owner.user_scope.to_storage(),p.ids.submission.to_storage(),p.ids.run.to_storage(),row.capture.canonical.config_revision,row.capture.canonical.policy_revision,serde_json::to_string(&intent).map_err(bad)?,stamp.millis(),String::from(p.semantic_hash)]).map_err(bad)?;
        if changed != 1 {
            return Err(BotError::Conflict);
        }
        let changed=tx.execute("UPDATE bot_routine_preparations SET stage='linked',linked_request_id=submission_request_id,linked_chat_id=chat_id,updated_at_ms=?5 WHERE user_scope_id=?1 AND owner_device_id=?2 AND schedule_id=?3 AND occurrence_ms=?4 AND stage='prepared' AND preparation_hash=?6",
            params![slot.owner.user_scope.to_storage(),slot.owner.device.to_storage(),slot.schedule.to_storage(),slot.occurrence_ms,stamp.millis(),p.preparation_hash.as_bytes().as_slice()]).map_err(bad)?;
        if changed != 1 {
            return Err(BotError::Conflict);
        }
        let turn = row.pending_turn(stamp, prompt_template_version)?;
        super::super::super::super::bot_replay::prepare_routine_association_on(
            &tx, &turn, p, stamp,
        )
        .map_err(|_| BotError::Storage)?;
        library::insert_library_capture_tx(&tx, &row.capture.library, stamp.millis())?;
        row.stage = BotRoutineStoredStage::Linked;
        row.updated_at_ms = stamp.millis();
        // Decode all repeated identities before commit, so any representation
        // mismatch rolls back the entire receipt/chat/messages/capture batch.
        let stored = rows::read(&tx, slot)?.ok_or(BotError::Storage)?;
        if stored != row {
            return Err(BotError::Storage);
        }
        tx.commit().map_err(bad)?;
        Ok(BotRoutinePreparationAdmission::Ready(Box::new(row)))
    }
}

fn settle_linked(
    conn: &Connection,
    row: &BotRoutineStoredPreparation,
    reason: RoutineTerminalReason,
    now: i64,
) -> Result<(), BotError> {
    let p = &row.prepared;
    super::super::super::super::bot_replay::settle_routine_attempt_on(
        conn,
        InferenceAttemptId::from_uuid(p.ids.attempt),
        reason,
        Timestamp::from_millis(now),
    )
    .map_err(|_| BotError::Storage)?;
    // Only an unacknowledged prepared receipt changes here. Existing accepted
    // or terminal history is immutable; delegate truth supplies terminal reason.
    let outcome = BotExecutionOutcome::Rejected {
        version: 1,
        reason: BotAdmissionRefusal::Rejected,
    };
    conn.execute("UPDATE bot_command_receipts SET stage='rejected',outcome_json=?3,updated_at_ms=MAX(updated_at_ms,?4) WHERE user_scope_id=?1 AND request_id=?2 AND command_kind='routine_occurrence' AND stage='prepared'",
        params![p.slot.owner.user_scope.to_storage(),p.ids.submission.to_storage(),serde_json::to_string(&outcome).map_err(bad)?,now]).map_err(bad)?;
    let released = conn.execute("UPDATE bots SET folder_lock_request_id=NULL WHERE bot_id=?1 AND folder_lock_request_id=?2 AND folder_frozen_at_ms IS NULL",params![p.slot.bot.to_storage(),p.ids.submission.to_storage()]).map_err(bad)?;
    if released != 0 {
        crate::repository::bots::client_feed::touch_bot(conn, p.slot.bot).map_err(bad)?;
    }
    Ok(())
}
